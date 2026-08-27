//! Plays a cutscene's own camera over the level its `CTDS` names: the shots each `CTTL` states,
//! sequenced in the order the file lists them since nothing else states another order.
//!
//! The camera comes from `C004` plus the `TMFC` curve set its `curve_id` names. Target 1 of that
//! set is the eye - its own translate and rotate curves are where the shot actually moves; see the
//! ironworks `C004` doc for the camera's own focal length and roll fields, on the set's target
//! `0xff`. Actors are not played: a `CTAL` participant only gets a marker, from [`markers`].

use std::cell::RefCell;

use egui::{Align, Button, CentralPanel, Layout, RichText, ScrollArea, containers::panel::Panel};
use glam::{EulerRot, Quat, Vec3};
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::lvb::LevelFile;
use ironworks::file::tmb::{Channel, CommandKind, Curves, Item, Timeline};

use crate::assets::viewers::layer;
use crate::assets::viewers::layer::scene;
use crate::backend::Backend;
use crate::data::FileProviderExt;
use crate::utils::{PromiseKind, TrackedPromise};

/// The curve target carrying the eye's own position and rotation, measured against the corpus.
/// Contradicts the ironworks crate's own doc comment on `C004`, which names target 2: that target
/// carries curves of the same shape but sits at the identity in every real shot sampled, where
/// target 1 carries the shot's actual blocking.
const EYE: u8 = 1;

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

/// The eye's forward and up at a rotation (degrees, turning about X first, then Y, then Z - the
/// same order every other curve-driven transform in this crate reads) plus a roll about its own
/// forward axis, applied the other way round from how the file states it.
fn banked_basis(rotation_deg: Vec3, roll_deg: f32) -> (Vec3, Vec3) {
    let facing = Quat::from_euler(
        EulerRot::XYZ,
        rotation_deg.x.to_radians(),
        rotation_deg.y.to_radians(),
        rotation_deg.z.to_radians(),
    );
    let banked = facing * Quat::from_rotation_z((-roll_deg).to_radians());
    (banked * Vec3::NEG_Z, banked * Vec3::Y)
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

/// The eye's pose at a time within the shot's own span, held past either end the way
/// [`ironworks::file::tmb::Curve::at`] holds a curve.
fn eye_pose(set: &Curves, time: f32, near: f32, far: f32) -> Pose {
    let position = Vec3::new(
        curve_value(set, EYE, Channel::TranslationX, time),
        curve_value(set, EYE, Channel::TranslationY, time),
        curve_value(set, EYE, Channel::TranslationZ, time),
    );
    let rotation = Vec3::new(
        curve_value(set, EYE, Channel::RotationX, time),
        curve_value(set, EYE, Channel::RotationY, time),
        curve_value(set, EYE, Channel::RotationZ, time),
    );
    let roll = camera_field(set, ROLL_TAG, time).unwrap_or(0.0);
    let (forward, up) = banked_basis(rotation, roll);
    let fov_degrees = camera_field(set, FOCAL_LENGTH_TAG, time)
        .filter(|focal| *focal > 0.0)
        .map(field_of_view_degrees)
        .unwrap_or(55.0);
    Pose { position, forward, up, fov_degrees, near, far }
}

/// One shot: a `C004` command and the `CTTL` node it came from, in the cutscene's own global
/// frame numbering (its segment's own start added on).
pub struct Shot {
    pub node: usize,
    pub name: Option<String>,
    pub start: f32,
    pub duration: f32,
    curves: i16,
    near: f32,
    far: f32,
}

/// The command ids a timeline's own actors and tracks reach, so a shot nothing plays is told apart
/// from one its own structure never offers. Empty where the timeline names no actors at all, which
/// a filter reads as "nothing is excluded" rather than "everything is".
fn reachable_commands(timeline: &Timeline) -> std::collections::BTreeSet<i16> {
    let mut reachable = std::collections::BTreeSet::new();
    for item in timeline.items() {
        let Item::ActorList(list) = item else { continue };
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
/// runs, its name, which `TMFC` drives it, and its stated clip planes.
type RawShot = (i16, f32, f32, Option<String>, i16, f32, f32);

/// The `C004` shots one `CTTL` holds, filtered to the ones its own actor tracks reach where any
/// are, in the order they run.
fn shots_of(timeline: &Timeline) -> Vec<RawShot> {
    let reachable = reachable_commands(timeline);
    let all: Vec<RawShot> = timeline
        .items()
        .iter()
        .filter_map(|item| {
            let Item::Command(command) = item else { return None };
            let CommandKind::C004(camera) = command.kind() else { return None };
            Some((
                command.id(),
                f32::from(command.time()),
                camera.duration().max(0) as f32,
                camera.name().map(str::to_owned),
                camera.curve_id().try_into().unwrap_or(0),
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
            let Node::Timeline(timeline) = held else { continue };
            let local = shots_of(timeline);
            let span = timeline_span(
                timeline,
                &local
                    .iter()
                    .map(|(id, start, duration, ..)| (*id, *start, *duration))
                    .collect::<Vec<_>>(),
            );
            for (_, start, duration, name, curves, near, far) in local {
                shots.push(Shot {
                    node,
                    name,
                    start: offset + start,
                    duration,
                    curves,
                    near,
                    far,
                });
            }
            offset += span.max(1.0);
        }
        Self { duration: offset, shots }
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
        Some(eye_pose(set, time - shot.start, shot.near, shot.far))
    }
}

/// The `CTAL` participants a cutscene names, as points to mark rather than characters to draw.
pub fn markers(cutscene: &Cutscene) -> Vec<(Vec3, String)> {
    cutscene
        .nodes()
        .iter()
        .find_map(|node| match node {
            Node::Participants(participants) => Some(participants),
            _ => None,
        })
        .map(|participants| {
            participants
                .iter()
                .map(|participant| {
                    (
                        Vec3::from_array(participant.position()),
                        format!("{:#010x}", participant.id()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
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
    time: f32,
    playing: bool,
    /// Frames a second. No file states one for a cutscene; this is a starting guess the transport
    /// bar can move.
    fps: f32,
}

impl Default for State {
    fn default() -> Self {
        Self { fetch: Fetch::Idle, time: 0.0, playing: false, fps: DEFAULT_FPS }
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
        Self { level, player: Player::new(cutscene), state: RefCell::new(State::default()) }
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
            Ok(file) => Fetch::Ready(Box::new(layer::level_scene(&tab.level, file))),
            Err(error) => Fetch::Failed(error.to_string()),
        };
    }

    Panel::left("cutb_shots").default_size(200.0).show(ui, |ui| {
        shots_ui(ui, tab, &mut state);
    });
    Panel::bottom("cutb_transport").show(ui, |ui| {
        ui.add_space(4.0);
        transport(ui, tab, &mut state);
        ui.add_space(4.0);
    });
    let time = state.time;
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
            if let Some(pose) = tab.player.pose_at(cutscene, time) {
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
    ScrollArea::vertical().id_salt("cutb_shot_list").auto_shrink(false).show(ui, |ui| {
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

fn transport(ui: &mut egui::Ui, tab: &Tab, state: &mut State) {
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
        if ui.add(Button::new(if state.playing { "⏸" } else { "▶" })).clicked() {
            state.playing = !state.playing;
        }
        ui.spacing_mut().slider_width = 200.0;
        ui.add(egui::Slider::new(&mut state.time, 0.0..=duration.max(1.0)).text("frame"));
        ui.add(egui::Slider::new(&mut state.fps, 5.0..=60.0).text("fps")).on_hover_text(
            "How fast to play the cutscene's own frames. No file states a rate for one; this is a \
             starting guess.",
        );
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
    fn identity_rotation_faces_local_negative_z() {
        let (forward, up) = banked_basis(Vec3::ZERO, 0.0);
        assert!(close(forward, Vec3::NEG_Z));
        assert!(close(up, Vec3::Y));
    }

    #[test]
    fn a_yaw_of_a_quarter_turn_faces_local_negative_x() {
        let (forward, _) = banked_basis(Vec3::new(0.0, 90.0, 0.0), 0.0);
        assert!(close(forward, Vec3::NEG_X));
    }

    #[test]
    fn roll_turns_up_but_not_forward() {
        let (forward, up) = banked_basis(Vec3::ZERO, 90.0);
        assert!(close(forward, Vec3::NEG_Z));
        // "The other way round": a positive roll field turns up towards +X, not -X.
        assert!(close(up, Vec3::X));
    }

    fn shot(start: f32, duration: f32) -> Shot {
        Shot { node: 0, name: None, start, duration, curves: 0, near: 0.1, far: 1000.0 }
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
