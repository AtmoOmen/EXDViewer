//! Plays a cutscene's own camera over the level its `CTDS` names: the shots each `CTTL` states,
//! sequenced in the order the file lists them since nothing else states another order.
//!
//! The camera comes from `C004` plus the `TMFC` curve set its `curve_id` names. Its targets carry a
//! role apiece and hang off one another: the eye stands where the last [`EYE`] target does, aimed
//! at the last [`LOOK_AT`] one with the last [`UP`] one over it, and each of those rides whichever
//! `CTAL` participant the shot's own bindings name. See the ironworks `C004` doc for the rest,
//! including the focal length and roll fields on the set's target `0xff`. Actors are not played: a
//! participant only gets a marker naming what it stands for, from [`markers`].

use std::cell::RefCell;
use std::collections::BTreeMap;

use egui::{Align, Button, CentralPanel, Layout, RichText, ScrollArea, containers::panel::Panel};
use glam::{Mat4, Quat, Vec3};
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::layer::{HelperKind, HelperObject, Instance, InstanceData, Transform};
use ironworks::file::lvb::LevelFile;
use ironworks::file::tmb::{Channel, CommandKind, Curves, Item, Timeline};

use crate::assets::viewers::layer;
use crate::assets::viewers::layer::scene;
use crate::backend::Backend;
use crate::character::stand;
use crate::data::FileProviderExt;
use crate::utils::{PromiseKind, TrackedPromise};

/// The roles a camera's curve set gives its targets. The frame the rest hang off is role 1, which
/// the shot binds but the camera never reads a position off.
const EYE: u8 = 2;
const LOOK_AT: u8 = 3;
const UP: u8 = 4;

/// Which `C004` binding names each role's participant, and where the flag holding role 1 to a
/// participant's position alone sits.
const ROLES: [(u8, usize); 3] = [(1, 0), (EYE, 6), (LOOK_AT, 11)];
const RIG_UPRIGHT: usize = 4;

/// How far a target's parents are followed, past which a file naming a loop of them stops rather
/// than hangs. Deeper than any set the game ships.
const DEPTH: u8 = 16;

/// Where a set's own fields sit, past its targets' transform channels.
const CAMERA_FIELDS: u8 = 0xFF;
const FOCAL_LENGTH_TAG: u8 = 0x34;
const ROLL_TAG: u8 = 0x35;

/// Half the sensor height the game turns a focal length into a vertical field of view against, at
/// a frame it fixes at sixteen by nine.
const HALF_SENSOR: f32 = 7.001_51;

/// No file states a frame rate for a cutscene's own timing; this is a starting guess the transport
/// bar can move.
const DEFAULT_FPS: f32 = 30.0;

/// A camera pose, already in world space and degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub position: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
    pub fov_degrees: f32,
    pub near: f32,
    pub far: f32,
}

impl Pose {
    pub fn drive(self) -> scene::Drive {
        scene::Drive {
            position: self.position,
            forward: self.forward,
            up: self.up,
            fov_degrees: self.fov_degrees,
            near: self.near,
            far: self.far,
        }
    }
}

/// The vertical field of view a focal length turns into, in degrees.
fn field_of_view_degrees(focal_mm: f32) -> f32 {
    (2.0 * (HALF_SENSOR / focal_mm).atan()).to_degrees()
}

/// The up vector a roll leaves, turned about the eye's own forward axis. A positive roll takes it
/// towards the eye's right, which is the other way round from how the file states it.
fn banked(forward: Vec3, up: Vec3, roll_deg: f32) -> Vec3 {
    Quat::from_axis_angle(forward, roll_deg.to_radians()) * up
}

fn curve_value(set: &Curves, target: u8, channel: Channel, time: f32) -> f32 {
    set.channel(target, channel)
        .and_then(|curve| curve.at(time))
        .unwrap_or(0.0)
}

fn camera_field(set: &Curves, tag: u8, time: f32) -> Option<f32> {
    set.curves()
        .iter()
        .find(|curve| curve.target() == CAMERA_FIELDS && curve.tag() & 0x3F == tag)
        .and_then(|curve| curve.at(time))
}

/// One target of a camera's curve set: what it stands for, what it hangs off, and where the
/// participant its role binds stands, with whether it turns with that participant as well.
struct Target {
    role: u8,
    parent: Option<u8>,
    bound: Option<(Mat4, bool)>,
}

/// The targets a shot's curve set drives, with each role's binding resolved onto the first target
/// carrying it - the only one the shot binds.
fn rig(set: &Curves, bindings: &[u32; 17], participants: &[Instance]) -> BTreeMap<u8, Target> {
    let mut targets = BTreeMap::new();
    for curve in set.curves().iter().filter(|curve| curve.target() != CAMERA_FIELDS) {
        targets.entry(curve.target()).or_insert(Target {
            role: curve.role(),
            parent: curve.parent(),
            bound: None,
        });
    }
    for (role, slot) in ROLES {
        // The second participant of a pair stands in where the first names nothing: the game skips
        // a role's binding only when neither of the two resolves.
        let Some(participant) = [bindings[slot], bindings[slot + 2]]
            .into_iter()
            .find_map(|id| participants.iter().find(|held| held.id() == id))
        else {
            continue;
        };
        let Some(target) = targets.values_mut().find(|target| target.role == role) else {
            continue;
        };
        let turns = role == ROLES[0].0 && bindings[RIG_UPRIGHT] != 1;
        target.bound = Some((scene::matrix(stands_at(participant)), turns));
    }
    targets
}

/// Where a target stands at a time, in the frame its parents and its own binding put it in. A
/// bound target that does not turn keeps its parent's facing and takes only the participant's
/// position.
fn world(
    set: &Curves,
    targets: &BTreeMap<u8, Target>,
    index: u8,
    time: f32,
    depth: u8,
) -> Mat4 {
    let Some(target) = targets.get(&index) else {
        return Mat4::IDENTITY;
    };
    let channels = |channels: [Channel; 3]| {
        Vec3::from_array(channels.map(|channel| curve_value(set, index, channel, time)))
    };
    let local = Mat4::from_rotation_translation(
        Quat::from_mat3(&scene::rotation(
            channels([Channel::RotationX, Channel::RotationY, Channel::RotationZ])
                .to_array()
                .map(f32::to_radians),
        )),
        channels([
            Channel::TranslationX,
            Channel::TranslationY,
            Channel::TranslationZ,
        ]),
    );
    let parent = match target.parent.filter(|_| depth < DEPTH) {
        Some(parent) => world(set, targets, parent, time, depth + 1),
        None => Mat4::IDENTITY,
    };
    frame(parent, target.bound) * local
}

/// The frame a target's own channels sit in: the placement its role binds, whole where the role
/// turns with the participant and as its position over the parent's own facing where it does not.
fn frame(parent: Mat4, bound: Option<(Mat4, bool)>) -> Mat4 {
    match bound {
        Some((placement, true)) => placement,
        Some((placement, false)) => Mat4::from_cols(
            parent.x_axis,
            parent.y_axis,
            parent.z_axis,
            placement.w_axis,
        ),
        None => parent,
    }
}

/// Where the last target of a role stands, which is the one the camera reads: the game walks its
/// targets from the end.
fn stands(set: &Curves, targets: &BTreeMap<u8, Target>, role: u8, time: f32) -> Option<Vec3> {
    let index = *targets
        .iter()
        .rev()
        .find(|(_, target)| target.role == role)?
        .0;
    Some(world(set, targets, index, time, 0).w_axis.truncate())
}

/// The camera's pose at a time within the shot's own span, held past either end the way
/// [`ironworks::file::tmb::Curve::at`] holds a curve.
fn eye_pose(
    set: &Curves,
    targets: &BTreeMap<u8, Target>,
    time: f32,
    near: f32,
    far: f32,
) -> Option<Pose> {
    let position = stands(set, targets, EYE, time)?;
    let forward = (stands(set, targets, LOOK_AT, time)? - position).normalize_or_zero();
    let up = stands(set, targets, UP, time)
        .map(|over| over - position)
        .unwrap_or(Vec3::Y);
    let roll = camera_field(set, ROLL_TAG, time).unwrap_or(0.0);
    let fov_degrees = camera_field(set, FOCAL_LENGTH_TAG, time)
        .filter(|focal| *focal > 0.0)
        .map(field_of_view_degrees)
        .unwrap_or(55.0);
    Some(Pose {
        position,
        forward,
        up: banked(forward, up, roll),
        fov_degrees,
        near,
        far,
    })
}

/// One shot: a `C004` command and the `CTTL` node it came from, in the cutscene's own global
/// frame numbering (its segment's own start added on).
pub struct Shot {
    pub node: usize,
    pub name: Option<String>,
    pub start: f32,
    pub duration: f32,
    curves: i16,
    bindings: [u32; 17],
    near: f32,
    far: f32,
}

/// The command ids a timeline's own actors and tracks reach, so a shot nothing plays is told apart
/// from one its own structure never offers. Empty where the timeline names no actors at all, which
/// a filter reads as "nothing is excluded" rather than "everything is".
fn reachable_commands(timeline: &Timeline) -> std::collections::BTreeSet<i16> {
    let mut reachable = std::collections::BTreeSet::new();
    for item in timeline.items() {
        let Item::ActorList(list) = item else {
            continue;
        };
        for actor_id in list.actors() {
            let Some(Item::Actor(actor)) = timeline
                .items()
                .iter()
                .find(|item| matches!(item, Item::Actor(a) if a.id() == *actor_id))
            else {
                continue;
            };
            for track_id in actor.tracks() {
                let Some(Item::Track(track)) = timeline
                    .items()
                    .iter()
                    .find(|item| matches!(item, Item::Track(t) if t.id() == *track_id))
                else {
                    continue;
                };
                reachable.extend(track.commands());
            }
        }
    }
    reachable
}

/// How long a `CTTL` plays for: what its own header states, or where nothing does, past the
/// furthest a shot it holds runs.
fn timeline_span(timeline: &Timeline, shots: &[(i16, f32, f32)]) -> f32 {
    let stated = timeline.items().iter().find_map(|item| match item {
        Item::Header(header) => Some(f32::from(header.duration())),
        _ => None,
    });
    stated.unwrap_or_else(|| {
        shots
            .iter()
            .map(|(_, start, duration)| start + duration)
            .fold(0.0, f32::max)
    })
}

/// One `C004` read out of a timeline: its own id (for the reachability filter), when it starts and
/// runs, its name, which `TMFC` drives it, what it binds, and its stated clip planes.
type RawShot = (i16, f32, f32, Option<String>, i16, [u32; 17], f32, f32);

/// The `C004` shots one `CTTL` holds, filtered to the ones its own actor tracks reach where any
/// are, in the order they run.
fn shots_of(timeline: &Timeline) -> Vec<RawShot> {
    let reachable = reachable_commands(timeline);
    let all: Vec<RawShot> = timeline
        .items()
        .iter()
        .filter_map(|item| {
            let Item::Command(command) = item else {
                return None;
            };
            let CommandKind::C004(camera) = command.kind() else {
                return None;
            };
            Some((
                command.id(),
                f32::from(command.time()),
                camera.duration().max(0) as f32,
                camera.name().map(str::to_owned),
                camera.curve_id().try_into().unwrap_or(0),
                *camera.bindings(),
                camera.near_plane(),
                camera.far_plane(),
            ))
        })
        .collect();
    let mut kept: Vec<_> = all
        .iter()
        .filter(|(id, ..)| reachable.is_empty() || reachable.contains(id))
        .cloned()
        .collect();
    if kept.is_empty() {
        kept = all;
    }
    kept.sort_by(|a, b| a.1.total_cmp(&b.1));
    kept
}

/// The shot active at a time, the last one to start at or before it. `None` before the first shot
/// anywhere in the cutscene has started.
fn active_shot(shots: &[Shot], time: f32) -> Option<&Shot> {
    shots
        .iter()
        .filter(|shot| shot.start <= time)
        .max_by(|a, b| a.start.total_cmp(&b.start))
}

/// A cutscene's camera, sequenced. Holds no bytes of its own past the shot list: [`Self::pose_at`]
/// reads the curves back out of the `Cutscene` it was built from.
pub struct Player {
    shots: Vec<Shot>,
    duration: f32,
}

impl Player {
    pub fn new(cutscene: &Cutscene) -> Self {
        let mut shots = Vec::new();
        let mut offset = 0.0;
        for (node, held) in cutscene.nodes().iter().enumerate() {
            let Node::Timeline(timeline) = held else {
                continue;
            };
            let local = shots_of(timeline);
            let span = timeline_span(
                timeline,
                &local
                    .iter()
                    .map(|(id, start, duration, ..)| (*id, *start, *duration))
                    .collect::<Vec<_>>(),
            );
            for (_, start, duration, name, curves, bindings, near, far) in local {
                shots.push(Shot {
                    node,
                    name,
                    start: offset + start,
                    duration,
                    curves,
                    bindings,
                    near,
                    far,
                });
            }
            offset += span.max(1.0);
        }
        Self {
            duration: offset,
            shots,
        }
    }

    /// Every shot, in the order it plays.
    pub fn shots(&self) -> &[Shot] {
        &self.shots
    }

    /// How long the whole cutscene plays for, in frames.
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// The camera at a time, or `None` before any shot has started.
    pub fn pose_at(&self, cutscene: &Cutscene, time: f32) -> Option<Pose> {
        let shot = active_shot(&self.shots, time)?;
        let Some(Node::Timeline(timeline)) = cutscene.nodes().get(shot.node) else {
            return None;
        };
        let set = timeline.items().iter().find_map(|item| match item {
            Item::Curves(held) if held.id() == shot.curves => Some(held),
            _ => None,
        })?;
        let targets = rig(set, &shot.bindings, participants(cutscene));
        eye_pose(set, &targets, time - shot.start, shot.near, shot.far)
    }
}

/// The helper a participant is written as, where it is one.
fn helper(participant: &Instance) -> Option<&HelperObject> {
    match participant.data() {
        InstanceData::HelperObject(helper) => Some(helper),
        _ => None,
    }
}

/// What a participant stands for, in as few words as its record states.
pub fn stands_for(participant: &Instance) -> String {
    let Some(helper) = helper(participant) else {
        return format!("{:?}", participant.kind());
    };
    match helper.kind() {
        HelperKind::EventNpc | HelperKind::BattleNpc => {
            format!("{:?} {}", helper.kind(), helper.base_id())
        }
        HelperKind::Weapon => format!("Weapon {}", helper.weapon().pattern_id()),
        kind => helper
            .nested()
            .and_then(|nested| layer::asset(nested.data()))
            .and_then(|asset| asset.rsplit('/').next())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{kind:?}")),
    }
}

/// Whether a kind's own setup reads the placement stated beside it. The rest are built by copying
/// the participant record's header wholesale, transform and all, so the placement never reaches
/// them: `sub_141B26310` calls the placement-aware `sub_141B282F0` for every other kind, and for
/// these a plain copy of the record's first 0x30 bytes.
fn takes_placement(kind: HelperKind) -> bool {
    !matches!(
        kind,
        HelperKind::BgPart | HelperKind::SharedGroup | HelperKind::Weapon | HelperKind::Unknown85
    )
}

/// Where a participant stands: the transform its record states apart from the instance's own wins
/// where the flag says so and the kind reads it, the way the game's own setup takes it.
fn stands_at(participant: &Instance) -> Transform {
    helper(participant)
        .filter(|helper| takes_placement(helper.kind()))
        .and_then(HelperObject::placement)
        .filter(|placement| placement.flags() & 1 != 0)
        .map(|placement| placement.transform())
        .unwrap_or_else(|| participant.transform())
}

/// What a prop participant draws itself from, where its nested instance names one. Only the two
/// kinds that build background out of it: `Unknown85` names a nested shared group as well, and its
/// own setup takes the path alone, as a kind of instance this view has no notion of.
fn drawn_from(participant: &Instance) -> Option<scene::Asset> {
    let helper = helper(participant)
        .filter(|helper| matches!(helper.kind(), HelperKind::BgPart | HelperKind::SharedGroup))?;
    let asset = match helper.nested()?.data() {
        InstanceData::BgPart(part) => scene::Asset::Model(part.asset_path().clone()),
        InstanceData::SharedGroup(group) => scene::Asset::Group(group.asset_path().clone()),
        _ => return None,
    };
    let (scene::Asset::Model(path) | scene::Asset::Group(path)) = &asset;
    (!path.is_empty()).then_some(asset)
}

/// The scenery a cutscene brings with it: the participants naming a model or a shared group, at the
/// transforms their own records state. The nested instance carries the asset and nothing else - its
/// own transform is all zeroes in every shipping file.
fn props(cutscene: &Cutscene) -> Vec<scene::Prop> {
    participants(cutscene)
        .iter()
        .filter_map(|participant| {
            Some(scene::Prop {
                asset: drawn_from(participant)?,
                transform: stands_at(participant),
                id: participant.id(),
            })
        })
        .collect()
}

/// The character each participant stands for, with no live one on hand to copy. `sub_141B26310`
/// takes the live character its kind names and falls back to a row: a party member to one fixed
/// stand-in unless the record forces an id of its own, a stabled chocobo to one whichever id it
/// names, and the player to the record's own - which every shipping file leaves at a row stating
/// no race, no equipment and no `ModelChara`, so nothing is drawn for one.
fn stands_as(participant: &Instance) -> Option<stand::Wanted> {
    let helper = helper(participant)?;
    let (roll, id) = match helper.kind() {
        HelperKind::EventNpc | HelperKind::Player => (stand::Roll::Event, helper.base_id()),
        HelperKind::BattleNpc => (stand::Roll::Battle, helper.base_id()),
        HelperKind::PartyMember | HelperKind::PartyMemberAlt | HelperKind::Unknown82 => (
            stand::Roll::Event,
            match helper.forces_base_id() {
                true => helper.base_id(),
                false => stand::PARTY_STAND_IN,
            },
        ),
        HelperKind::StableChocobo => (stand::Roll::Event, stand::STABLED_CHOCOBO),
        _ => return None,
    };
    (id != 0).then(|| stand::Wanted {
        roll,
        id,
        height: helper.height(),
        at: stands_at(participant),
    })
}

/// Everyone a cutscene stands, at the transforms their own records state.
fn cast(cutscene: &Cutscene) -> Vec<stand::Wanted> {
    participants(cutscene).iter().filter_map(stands_as).collect()
}

/// What a `CTAL` holds, as a count of each kind its participants stand for.
pub fn roll_call(participants: &[Instance]) -> String {
    let mut held: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for participant in participants {
        let named = match helper(participant) {
            Some(helper) => format!("{:?}", helper.kind()),
            None => format!("{:?}", participant.kind()),
        };
        *held.entry(named).or_default() += 1;
    }
    let mut lines: Vec<(usize, String)> = held
        .into_iter()
        .map(|(named, count)| (count, named))
        .collect();
    lines.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    lines
        .iter()
        .take(4)
        .map(|(count, named)| format!("{count} {named}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `CTAL` a cutscene holds, empty where it names none.
fn participants(cutscene: &Cutscene) -> &[Instance] {
    cutscene
        .nodes()
        .iter()
        .find_map(|node| match node {
            Node::Participants(participants) => Some(participants.as_slice()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The `CTAL` participants a cutscene names, as points to mark rather than characters to draw.
pub fn markers(cutscene: &Cutscene) -> Vec<(Vec3, String)> {
    participants(cutscene)
        .iter()
        .map(|participant| {
            (
                Vec3::from_array(stands_at(participant).translation()),
                format!("{} · {:#x}", stands_for(participant), participant.id()),
            )
        })
        .collect()
}

/// The `.lvb` a `CTDS` names its level by: the same shape the Assets tab's own Zones tab resolves.
fn level_path(level: &str) -> String {
    format!("bg/{level}.lvb")
}

enum Fetch {
    Idle,
    Loading(Box<TrackedPromise<anyhow::Result<LevelFile>>>),
    Ready(Box<scene::Scene>),
    Failed(String),
}

struct State {
    fetch: Fetch,
    /// Everyone standing in the scene, from the rows their participants name through to the models
    /// the scene draws.
    cast: stand::Cast,
    time: f32,
    playing: bool,
    /// Frames a second. No file states one for a cutscene; this is a starting guess the transport
    /// bar can move.
    fps: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            fetch: Fetch::Idle,
            cast: stand::Cast::default(),
            time: 0.0,
            playing: false,
            fps: DEFAULT_FPS,
        }
    }
}

/// A cutscene's own "Play" tab: the level its `CTDS` names, with the camera driven by its shots
/// instead of the free orbit camera.
pub struct Tab {
    level: String,
    player: Player,
    state: RefCell<State>,
}

impl Tab {
    pub fn new(level: String, cutscene: &Cutscene) -> Self {
        Self {
            level,
            player: Player::new(cutscene),
            state: RefCell::new(State::default()),
        }
    }
}

pub fn ui(ui: &mut egui::Ui, tab: &Tab, cutscene: &Cutscene, backend: &Backend) -> Option<String> {
    if tab.level.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("This cutscene names no level").weak());
        });
        return None;
    }

    let mut state = tab.state.borrow_mut();
    if matches!(&state.fetch, Fetch::Idle) {
        let files = backend.files().clone();
        let path = level_path(&tab.level);
        state.fetch = Fetch::Loading(Box::new(TrackedPromise::spawn_local(async move {
            files.file::<LevelFile>(&path).await
        })));
    }
    if matches!(&state.fetch, Fetch::Loading(promise) if promise.try_get().is_some()) {
        let Fetch::Loading(promise) = std::mem::replace(&mut state.fetch, Fetch::Idle) else {
            unreachable!()
        };
        state.fetch = match promise.block_and_take() {
            Ok(file) => {
                let mut scene = layer::level_scene(&tab.level, file);
                scene.place("Cutscene", props(cutscene));
                state.cast = stand::Cast::new(cast(cutscene));
                Fetch::Ready(Box::new(scene))
            }
            Err(error) => Fetch::Failed(error.to_string()),
        };
    }

    let pose = tab.player.pose_at(cutscene, state.time);
    state.cast.poll(ui.ctx(), backend);
    let standing = state.cast.standing();

    Panel::left("cutb_shots")
        .default_size(200.0)
        .show(ui, |ui| {
            shots_ui(ui, tab, &mut state);
        });
    Panel::bottom("cutb_transport").show(ui, |ui| {
        ui.add_space(4.0);
        transport(ui, tab, &mut state, pose.as_ref());
        ui.add_space(4.0);
    });
    CentralPanel::default().show(ui, |ui| match &mut state.fetch {
        Fetch::Idle | Fetch::Loading(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the level…");
            });
        }
        Fetch::Failed(error) => {
            ui.colored_label(egui::Color32::RED, error.clone());
        }
        Fetch::Ready(scene) => {
            scene.stand(standing);
            if let Some(pose) = pose {
                scene.drive(pose.drive());
            }
            scene.mark(markers(cutscene));
            scene::ui(ui, scene, backend);
        }
    });
    None
}

fn shots_ui(ui: &mut egui::Ui, tab: &Tab, state: &mut State) {
    let active = active_shot(tab.player.shots(), state.time).map(|shot| shot.start);
    ScrollArea::vertical()
        .id_salt("cutb_shot_list")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                for shot in tab.player.shots() {
                    let current = active == Some(shot.start);
                    let label = format!(
                        "{} · node {} · {:.0}f",
                        shot.name.as_deref().unwrap_or("-"),
                        shot.node,
                        shot.duration,
                    );
                    if ui.add(Button::selectable(current, label)).clicked() {
                        state.time = shot.start;
                        state.playing = false;
                    }
                }
                if tab.player.shots().is_empty() {
                    ui.label(RichText::new("This cutscene's timelines hold no camera").weak());
                }
            });
        });
}

fn transport(ui: &mut egui::Ui, tab: &Tab, state: &mut State, pose: Option<&Pose>) {
    let duration = tab.player.duration();
    if state.playing {
        state.time += ui.input(|input| input.stable_dt).min(0.25) * state.fps;
        if state.time >= duration {
            state.time = duration;
            state.playing = false;
        }
        ui.ctx().request_repaint();
    }

    ui.horizontal_wrapped(|ui| {
        if ui.button("⏮").on_hover_text("Back to the start").clicked() {
            state.time = 0.0;
            state.playing = false;
        }
        if ui
            .add(Button::new(if state.playing { "⏸" } else { "▶" }))
            .clicked()
        {
            state.playing = !state.playing;
        }
        ui.spacing_mut().slider_width = 200.0;
        ui.add(egui::Slider::new(&mut state.time, 0.0..=duration.max(1.0)).text("frame"));
        ui.add(egui::Slider::new(&mut state.fps, 5.0..=60.0).text("fps")).on_hover_text(
            "How fast to play the cutscene's own frames. No file states a rate for one; this is a \
             starting guess.",
        );
        ui.label(
            RichText::new(match pose {
                Some(pose) => format!(
                    "eye {:.1}, {:.1}, {:.1} · {:.1}\u{b0}",
                    pose.position.x, pose.position.y, pose.position.z, pose.fov_degrees
                ),
                None => "no shot active yet".to_owned(),
            })
            .weak(),
        );
        let (built, wanted) = state.cast.built();
        if wanted > 0 {
            ui.label(RichText::new(format!("{built}/{wanted} standing")).weak())
                .on_hover_text(
                    "Characters built out of the rows their participants name, against how many \
                     rows the cast holds",
                );
        }
        if let Some(why) = state.cast.failure() {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("cast: {why}"));
        }
    });
}

#[cfg(test)]
mod test {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn a_focal_length_of_the_half_sensor_gives_a_right_angle() {
        // atan(1) doubled is a quarter turn: the vertical frame exactly spans the lens.
        let fov = field_of_view_degrees(HALF_SENSOR);
        assert!((fov - 90.0).abs() < 1e-3);
    }

    #[test]
    fn a_longer_focal_length_narrows_the_field_of_view() {
        assert!(field_of_view_degrees(70.0) < field_of_view_degrees(35.0));
    }

    #[test]
    fn roll_turns_up_about_the_eye_s_own_forward() {
        assert!(close(banked(Vec3::NEG_Z, Vec3::Y, 0.0), Vec3::Y));
        // "The other way round": a positive roll field turns up towards +X, not -X.
        assert!(close(banked(Vec3::NEG_Z, Vec3::Y, 90.0), Vec3::X));
    }

    #[test]
    fn a_binding_that_does_not_turn_keeps_the_parent_s_facing() {
        let parent = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let placement = Mat4::from_rotation_translation(
            Quat::from_rotation_y(std::f32::consts::PI),
            Vec3::new(3.0, 4.0, 5.0),
        );
        let held = frame(parent, Some((placement, false)));
        assert!(close(held.w_axis.truncate(), Vec3::new(3.0, 4.0, 5.0)));
        assert!(close(
            held.transform_vector3(Vec3::Z),
            parent.transform_vector3(Vec3::Z)
        ));
        let turning = frame(parent, Some((placement, true)));
        assert!(close(turning.transform_vector3(Vec3::Z), Vec3::NEG_Z));
        assert!(close(turning.w_axis.truncate(), Vec3::new(3.0, 4.0, 5.0)));
    }

    fn shot(start: f32, duration: f32) -> Shot {
        Shot {
            node: 0,
            name: None,
            start,
            duration,
            curves: 0,
            bindings: [0xffff_ffff; 17],
            near: 0.1,
            far: 1000.0,
        }
    }

    #[test]
    fn the_active_shot_is_the_last_one_that_has_started() {
        let shots = vec![shot(0.0, 30.0), shot(30.0, 60.0), shot(120.0, 10.0)];
        assert!(active_shot(&shots, -1.0).is_none());
        assert_eq!(active_shot(&shots, 0.0).unwrap().start, 0.0);
        assert_eq!(active_shot(&shots, 45.0).unwrap().start, 30.0);
        // Past every shot's own duration, the last one to start still holds: a cut is what ends a
        // shot, not its own stated length.
        assert_eq!(active_shot(&shots, 200.0).unwrap().start, 120.0);
    }

    #[test]
    fn a_later_shot_starting_before_an_earlier_one_ends_preempts_it() {
        let shots = vec![shot(0.0, 100.0), shot(10.0, 5.0)];
        assert_eq!(active_shot(&shots, 50.0).unwrap().start, 10.0);
    }
}
