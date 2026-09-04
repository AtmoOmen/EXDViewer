//! What the `CTAL` participants of every shipping cutscene decode to, and whether what they name
//! resolves.
//!
//! `cutb_actors <paths file>`

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use ironworks::excel::{Excel, Language};
use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::layer::{HelperKind, Instance, InstanceData, InstanceKind, Transform};
use ironworks::file::lvb::LevelFile;
use ironworks::file::mdl::{Lod, MeshKind, ModelContainer};
use ironworks::file::{lcb, svb};
use ironworks::file::sgb::SharedGroupFile;
use ironworks::file::tmb::{Channel, CommandKind, Curves, Item};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

/// How deep a shared group is followed into another, matching the scene view's own cap.
const DEPTH: u8 = 8;

/// Whether a model holds geometry the scene view would draw, at the level it asks for first.
fn drawable(bytes: Vec<u8>) -> Option<usize> {
    meshes(bytes, Lod::High)
}

/// How many meshes a model holds that the scene view would draw, at one detail level.
fn meshes(bytes: Vec<u8>, level: Lod) -> Option<usize> {
    let container = ModelContainer::read(std::io::Cursor::new(bytes)).ok()?;
    let drawn = container
        .model(level)
        .meshes()
        .into_iter()
        .filter(|mesh| {
            mesh.kinds().iter().any(|kind| {
                matches!(
                    kind,
                    MeshKind::Standard
                        | MeshKind::Water
                        | MeshKind::LightShaft
                        | MeshKind::VerticalFog
                )
            }) && mesh.attributes().is_ok()
                && mesh.indices().is_ok()
        })
        .count();
    Some(drawn)
}

/// What a shared group places, following the groups it names in turn: the models it stands, and
/// how many effects, lights and sounds it brings alongside them.
fn parts(
    ironworks: &Ironworks,
    path: &str,
    depth: u8,
    into: &mut BTreeSet<String>,
    beside: &mut BTreeMap<&'static str, usize>,
) -> bool {
    if depth >= DEPTH {
        return true;
    }
    let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
        return false;
    };
    let Ok(file) = SharedGroupFile::read(std::io::Cursor::new(bytes)) else {
        return false;
    };
    for group in file.scene().layer_groups() {
        for layer in group.layers() {
            for instance in layer.instances() {
                match instance.data() {
                    InstanceData::BgPart(part)
                        if part.visible() && !part.asset_path().is_empty() =>
                    {
                        into.insert(part.asset_path().clone());
                    }
                    InstanceData::SharedGroup(shared) if !shared.asset_path().is_empty() => {
                        parts(ironworks, shared.asset_path(), depth + 1, into, beside);
                    }
                    InstanceData::Vfx(vfx) if !vfx.asset_path().is_empty() => {
                        *beside.entry("vfx").or_default() += 1;
                    }
                    InstanceData::Light(_) => *beside.entry("light").or_default() += 1,
                    InstanceData::Sound(_) => *beside.entry("sound").or_default() += 1,
                    _ => {}
                }
            }
        }
    }
    true
}

/// Whether a transform states anything other than the identity.
fn moved(transform: Transform) -> bool {
    transform.translation().iter().any(|axis| *axis != 0.0)
        || transform.rotation().iter().any(|angle| *angle != 0.0)
        || transform.scale().iter().any(|axis| *axis != 1.0)
}

/// The nested instance a prop participant draws itself from, where it has one.
fn nested_of(participant: &Instance) -> Option<&Instance> {
    let InstanceData::HelperObject(helper) = participant.data() else {
        return None;
    };
    matches!(helper.kind(), HelperKind::BgPart | HelperKind::SharedGroup)
        .then(|| helper.nested())
        .flatten()
}


/// Where a participant stands: the transform its record states apart from the instance's own wins
/// where the flag says so, the way the play tab takes it.
fn stands_at(participant: &Instance) -> Transform {
    let InstanceData::HelperObject(helper) = participant.data() else {
        return participant.transform();
    };
    helper
        .placement()
        .filter(|placement| placement.flags() & 1 != 0)
        .map(|placement| placement.transform())
        .unwrap_or_else(|| participant.transform())
}

/// The roles a `C004` camera's curve set gives its targets, and which of the command's bindings
/// names the participant each of the first three rides. Index 4 holds role 1 to a participant's
/// position alone.
const ROLES: [(u8, usize); 3] = [(1, 0), (EYE, 6), (LOOK_AT, 11)];
const RIG_UPRIGHT: usize = 4;
const EYE: u8 = 2;
const LOOK_AT: u8 = 3;
const UP: u8 = 4;

/// One target of a camera's curve set, with the placement its role binds and whether it turns with
/// that participant as well as standing on it.
struct Target {
    role: u8,
    parent: Option<u8>,
    bound: Option<(Mat4, bool)>,
}

fn placement(transform: Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(transform.scale()),
        Quat::from_mat3(
            &(glam::Mat3::from_rotation_z(transform.rotation()[2])
                * glam::Mat3::from_rotation_y(transform.rotation()[1])
                * glam::Mat3::from_rotation_x(transform.rotation()[0])),
        ),
        Vec3::from_array(transform.translation()),
    )
}

/// The targets a shot drives, each role's binding resolved onto the first target carrying it.
/// `placements` empty stands for the reading that ignores the bindings entirely.
fn rig(
    set: &Curves,
    bindings: &[u32; 17],
    placements: &BTreeMap<u32, Mat4>,
) -> BTreeMap<u8, Target> {
    let mut targets: BTreeMap<u8, Target> = BTreeMap::new();
    for curve in set.curves().iter().filter(|curve| curve.target() != 0xFF) {
        targets.entry(curve.target()).or_insert(Target {
            role: curve.role(),
            parent: curve.parent(),
            bound: None,
        });
    }
    for (role, slot) in ROLES {
        let Some(held) = placements.get(&bindings[slot]) else {
            continue;
        };
        let Some(target) = targets.values_mut().find(|target| target.role == role) else {
            continue;
        };
        target.bound = Some((*held, role == ROLES[0].0 && bindings[RIG_UPRIGHT] != 1));
    }
    targets
}

fn world(set: &Curves, targets: &BTreeMap<u8, Target>, index: u8, time: f32, depth: u8) -> Mat4 {
    let Some(target) = targets.get(&index) else {
        return Mat4::IDENTITY;
    };
    let value = |channel| {
        set.channel(index, channel)
            .and_then(|curve| curve.at(time))
            .unwrap_or(0.0)
    };
    let turn = |channel| f32::to_radians(value(channel));
    let local = Mat4::from_rotation_translation(
        Quat::from_mat3(
            &(glam::Mat3::from_rotation_z(turn(Channel::RotationZ))
                * glam::Mat3::from_rotation_y(turn(Channel::RotationY))
                * glam::Mat3::from_rotation_x(turn(Channel::RotationX))),
        ),
        Vec3::new(
            value(Channel::TranslationX),
            value(Channel::TranslationY),
            value(Channel::TranslationZ),
        ),
    );
    let parent = match target.parent.filter(|_| depth < DEPTH) {
        Some(parent) => world(set, targets, parent, time, depth + 1),
        None => Mat4::IDENTITY,
    };
    let frame = match target.bound {
        Some((held, true)) => held,
        Some((held, false)) => {
            Mat4::from_cols(parent.x_axis, parent.y_axis, parent.z_axis, held.w_axis)
        }
        None => parent,
    };
    frame * local
}

/// Where the last target of a role stands, which is the one the game reads.
fn stands(set: &Curves, targets: &BTreeMap<u8, Target>, role: u8, time: f32) -> Option<Vec3> {
    let index = *targets
        .iter()
        .rev()
        .find(|(_, target)| target.role == role)?
        .0;
    Some(world(set, targets, index, time, 0).w_axis.truncate())
}

/// What the shots of every cutscene put the camera against: how the eye reads with the shot's own
/// bindings applied, and how it reads with them ignored.
#[derive(Default)]
struct Cameras {
    shots: usize,
    bound: Vec<f32>,
    loose: Vec<f32>,
    aim: Vec<f32>,
    aim_loose: Vec<f32>,
    span: Vec<f32>,
    square: Vec<f32>,
}

impl Cameras {
    fn read(&mut self, file: &Cutscene, placements: &BTreeMap<u32, Mat4>, people: &[Vec3]) {
        for node in file.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            for item in timeline.items() {
                let Item::Command(command) = item else {
                    continue;
                };
                let CommandKind::C004(camera) = command.kind() else {
                    continue;
                };
                let Some(set) = timeline.items().iter().find_map(|item| match item {
                    Item::Curves(held) if i32::from(held.id()) == camera.curve_id() => Some(held),
                    _ => None,
                }) else {
                    continue;
                };
                self.shots += 1;
                if people.is_empty() {
                    continue;
                }
                for time in [0.0, camera.duration().max(0) as f32 * 0.5] {
                    for held in [true, false] {
                        let empty = BTreeMap::new();
                        let targets = rig(
                            set,
                            camera.bindings(),
                            match held {
                                true => placements,
                                false => &empty,
                            },
                        );
                        let (Some(eye), Some(look)) = (
                            stands(set, &targets, EYE, time),
                            stands(set, &targets, LOOK_AT, time),
                        ) else {
                            continue;
                        };
                        let subject = *people
                            .iter()
                            .min_by(|a, b| (eye - **a).length().total_cmp(&(eye - **b).length()))
                            .expect("a character");
                        let forward = (look - eye).normalize_or_zero();
                        let towards = (subject - eye).normalize_or_zero();
                        let (near, angle) = match held {
                            true => (&mut self.bound, &mut self.aim),
                            false => (&mut self.loose, &mut self.aim_loose),
                        };
                        near.push((eye - subject).length());
                        if forward.length() > 0.5 && towards.length() > 0.5 {
                            angle.push(forward.dot(towards).clamp(-1.0, 1.0).acos().to_degrees());
                        }
                        if !held {
                            continue;
                        }
                        let Some(over) = stands(set, &targets, UP, time) else {
                            continue;
                        };
                        self.span.push((over - eye).length());
                        let up = (over - eye).normalize_or_zero();
                        if up.length() > 0.5 && forward.length() > 0.5 {
                            self.square
                                .push(up.dot(forward).clamp(-1.0, 1.0).acos().to_degrees());
                        }
                    }
                }
            }
        }
    }
}

/// The value at a fraction along a sorted run.
fn along(held: &[f32], fraction: f32) -> f32 {
    match held.is_empty() {
        true => f32::NAN,
        false => held[((held.len() - 1) as f32 * fraction) as usize],
    }
}

fn quantiles(named: &str, held: &mut Vec<f32>) {
    held.sort_by(f32::total_cmp);
    println!(
        "  {named}: n={} p10 {:.2} p25 {:.2} p50 {:.2} p75 {:.2} p90 {:.2}",
        held.len(),
        along(held, 0.1),
        along(held, 0.25),
        along(held, 0.5),
        along(held, 0.75),
        along(held, 0.9),
    );
}

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks: Arc<Ironworks> =
        Arc::new(Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))));
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);
    let rows = |name: &str| -> BTreeSet<u32> {
        excel
            .sheet(name)
            .map(|sheet| sheet.into_iter().map(|row| row.row_id()).collect())
            .unwrap_or_default()
    };
    let event_npcs = rows("ENpcBase");
    let battle_npcs = rows("BNpcBase");

    let list = std::env::args().nth(1).expect("a paths file");
    let paths = std::fs::read_to_string(list).expect("the paths file");

    let mut files = 0usize;
    let mut failed = 0usize;
    let mut instances: BTreeMap<String, usize> = BTreeMap::new();
    let mut undecoded = 0usize;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut nested: BTreeMap<String, usize> = BTreeMap::new();
    let mut placements = 0usize;
    let mut heights: BTreeMap<u8, usize> = BTreeMap::new();
    let mut stated: [usize; 3] = [0; 3];
    let mut spread: BTreeMap<&str, usize> = BTreeMap::new();
    let mut flags: BTreeMap<u32, usize> = BTreeMap::new();
    let mut ids: BTreeMap<&str, [usize; 2]> = BTreeMap::new();
    let mut assets: [usize; 2] = [0, 0];
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut unresolved: BTreeSet<(String, u32)> = BTreeSet::new();

    let mut props: BTreeMap<&str, usize> = BTreeMap::new();
    let mut propless = 0usize;
    let mut prop_override = [0usize; 2];
    let mut nested_moved = 0usize;
    let mut invisible = 0usize;
    let mut shadowing: BTreeMap<String, usize> = BTreeMap::new();
    let mut spheres = [0usize; 2];
    let mut fading = 0usize;
    let mut models: BTreeMap<String, usize> = BTreeMap::new();
    let mut groups: BTreeMap<String, usize> = BTreeMap::new();
    let mut carrying: BTreeMap<String, usize> = BTreeMap::new();
    let mut busiest: Vec<(usize, String)> = Vec::new();
    let mut levels: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut clashes: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cameras = Cameras::default();

    let mut here;
    for path in paths.lines().filter(|path| path.ends_with(".cutb")) {
        here = 0usize;
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let file = match Cutscene::read(std::io::Cursor::new(bytes)) {
            Ok(file) => file,
            Err(error) => {
                failed += 1;
                println!("{path}: {error}");
                continue;
            }
        };
        files += 1;

        for node in file.nodes() {
            let Node::Participants(participants) = node else {
                continue;
            };
            for participant in participants {
                let read = !matches!(participant.data(), InstanceData::Unknown(_));
                *instances
                    .entry(format!("{:?} {}", participant.kind(), read))
                    .or_default() += 1;
                if participant.kind() != InstanceKind::HelperObject {
                    continue;
                }
                let InstanceData::HelperObject(helper) = participant.data() else {
                    undecoded += 1;
                    continue;
                };
                *kinds.entry(format!("{:?}", helper.kind())).or_default() += 1;
                *heights.entry(helper.height()).or_default() += 1;
                if let Some(placement) = helper.placement() {
                    placements += 1;
                    *flags.entry(placement.flags()).or_default() += 1;
                    if placement.flags() & 1 != 0 {
                        let own = participant.transform();
                        let held = placement.transform();
                        let gap = |left: [f32; 3], right: [f32; 3]| {
                            left.iter()
                                .zip(right)
                                .map(|(a, b)| (a - b).abs())
                                .fold(0.0f32, f32::max)
                        };
                        let far = gap(own.translation(), held.translation());
                        stated[usize::from(far > 0.001) + usize::from(far > 1.0)] += 1;
                        if gap(own.rotation(), held.rotation()) > 0.001 {
                            *spread.entry("rotation differs").or_default() += 1;
                        }
                        if gap(own.scale(), held.scale()) > 0.001 {
                            *spread.entry("scale differs").or_default() += 1;
                        }
                        if far <= 0.001 && own.translation().iter().any(|axis| *axis != 0.0) {
                            *spread.entry("same, and away from the origin").or_default() += 1;
                        }
                    }
                }

                let sheet = match helper.kind() {
                    HelperKind::BattleNpc => Some(("BNpcBase", &battle_npcs)),
                    HelperKind::None | HelperKind::Existing => None,
                    _ => Some(("ENpcBase", &event_npcs)),
                };
                if let (Some((name, held)), true) = (sheet, helper.base_id() != 0) {
                    let entry = ids.entry(name).or_default();
                    entry[usize::from(held.contains(&helper.base_id()))] += 1;
                    if !held.contains(&helper.base_id()) {
                        unresolved.insert((name.to_owned(), helper.base_id()));
                    }
                }

                if matches!(helper.kind(), HelperKind::BgPart | HelperKind::SharedGroup) {
                    let named = match helper.kind() {
                        HelperKind::BgPart => "BgPart",
                        _ => "SharedGroup",
                    };
                    *props.entry(named).or_default() += 1;
                    here += 1;
                    match nested_of(participant) {
                        None => propless += 1,
                        Some(inner) => {
                            nested_moved += usize::from(moved(inner.transform()));
                            {
                                let own = participant.transform();
                                let held = inner.transform();
                                let gap = |left: [f32; 3], right: [f32; 3]| {
                                    left.iter()
                                        .zip(right)
                                        .map(|(a, b)| (a - b).abs())
                                        .fold(0.0f32, f32::max)
                                };
                                let same = gap(own.translation(), held.translation()) <= 0.001
                                    && gap(own.rotation(), held.rotation()) <= 0.001
                                    && gap(own.scale(), held.scale()) <= 0.001;
                                *carrying
                                    .entry(
                                        match (
                                            same,
                                            held.scale() == [1.0, 1.0, 1.0],
                                            held.scale() == [0.0, 0.0, 0.0]
                                                && held.translation() == [0.0, 0.0, 0.0]
                                                && held.rotation() == [0.0, 0.0, 0.0],
                                        ) {
                                            (true, ..) => "nested states the participant's own",
                                            (false, true, _) => "nested differs, unit scale",
                                            (false, _, true) => "nested is all zeroes",
                                            (false, ..) => "nested differs, scaled",
                                        }
                                        .to_owned(),
                                    )
                                    .or_default() += 1;
                            }
                            if let InstanceData::BgPart(part) = inner.data() {
                                invisible += usize::from(!part.visible());
                                *shadowing
                                    .entry(format!("{:?}", part.world_light_shadow_mode()))
                                    .or_default() += 1;
                                spheres[usize::from(part.bounding_sphere_size() > 0.0)] += 1;
                                fading += usize::from(part.fade_out_distance() > 0.0);
                            }
                            match inner.data() {
                                InstanceData::BgPart(part) => {
                                    *models.entry(part.asset_path().clone()).or_default() += 1;
                                }
                                InstanceData::SharedGroup(group) => {
                                    *groups.entry(group.asset_path().clone()).or_default() += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(placement) = helper.placement().filter(|held| held.flags() & 1 != 0)
                    {
                        let own = participant.transform();
                        let held = placement.transform();
                        let gap = |left: [f32; 3], right: [f32; 3]| {
                            left.iter()
                                .zip(right)
                                .map(|(a, b)| (a - b).abs())
                                .fold(0.0f32, f32::max)
                        };
                        let differs = gap(own.translation(), held.translation()) > 0.001
                            || gap(own.rotation(), held.rotation()) > 0.001
                            || gap(own.scale(), held.scale()) > 0.001;
                        prop_override[usize::from(differs)] += 1;
                        let at_origin = |t: [f32; 3]| t.iter().all(|axis| *axis == 0.0);
                        if differs {
                            let named = match (
                                at_origin(own.translation()),
                                at_origin(held.translation()),
                            ) {
                                (true, false) => "participant at the origin, placement away",
                                (false, true) => "placement at the origin, participant away",
                                (true, true) => "both at the origin",
                                (false, false) => "both away from the origin",
                            };
                            *carrying.entry(named.to_owned()).or_default() += 1;
                            *carrying
                                .entry(format!(
                                    "  {named}: translation gap over a metre: {}",
                                    gap(own.translation(), held.translation()) > 1.0
                                ))
                                .or_default() += 1;
                        }
                    }
                }

                let Some(inner) = helper.nested() else {
                    continue;
                };
                *nested.entry(format!("{:?}", inner.kind())).or_default() += 1;
                for asset in [
                    match inner.data() {
                        InstanceData::BgPart(part) => Some(part.asset_path()),
                        InstanceData::SharedGroup(group) => Some(group.asset_path()),
                        _ => None,
                    },
                    match inner.data() {
                        InstanceData::BgPart(part) => Some(part.collision_asset_path()),
                        _ => None,
                    },
                ]
                .into_iter()
                .flatten()
                .filter(|asset| !asset.is_empty())
                {
                    let held = ironworks.file::<Vec<u8>>(asset).is_ok();
                    assets[usize::from(held)] += 1;
                    if !held {
                        missing.insert(asset.to_owned());
                    }
                }
            }
        }
        busiest.push((here, path.to_owned()));

        let Some(Node::Scene(scene)) = file
            .nodes()
            .iter()
            .find(|node| matches!(node, Node::Scene(_)))
        else {
            continue;
        };
        if scene.level().is_empty() {
            continue;
        }
        let level = format!("bg/{}.lvb", scene.level());
        let keys = levels.entry(level.clone()).or_insert_with(|| {
            let mut held = BTreeSet::new();
            let Ok(bytes) = ironworks.file::<Vec<u8>>(&level) else {
                return held;
            };
            let Ok(read) = LevelFile::read(std::io::Cursor::new(bytes)) else {
                return held;
            };
            let aside = |path: &String| match path.is_empty() {
                true => None,
                false => ironworks.file::<Vec<u8>>(path).ok(),
            };
            if let Some(bytes) = aside(read.scene().light_culling_path())
                && let Ok(boxes) = lcb::ClipBoxes::read(std::io::Cursor::new(bytes))
            {
                for group in boxes.groups() {
                    held.extend(group.entries().iter().map(lcb::Entry::instance));
                }
            }
            if let Some(bytes) = aside(read.scene().sky_visibility_path())
                && let Ok(sky) = svb::SkyVisibility::read(std::io::Cursor::new(bytes))
            {
                for group in sky.groups() {
                    held.extend(group.entries().iter().map(svb::Entry::instance));
                }
            }
            held
        });
        {
            let mut placements = BTreeMap::new();
            let mut people = Vec::new();
            for node in file.nodes() {
                let Node::Participants(participants) = node else {
                    continue;
                };
                for participant in participants {
                    let held = placement(stands_at(participant));
                    placements.insert(participant.id(), held);
                    let human = matches!(participant.data(), InstanceData::HelperObject(helper)
                        if matches!(
                            helper.kind(),
                            HelperKind::EventNpc | HelperKind::BattleNpc | HelperKind::Player
                        ));
                    // A metre off the participant's own feet, so the aim is measured against a
                    // character rather than the ground it stands on.
                    if human && held.w_axis.truncate().length() > 1.0 {
                        people.push(held.w_axis.truncate() + Vec3::Y);
                    }
                }
            }
            cameras.read(&file, &placements, &people);
        }

        for node in file.nodes() {
            let Node::Participants(participants) = node else {
                continue;
            };
            for participant in participants {
                if nested_of(participant).is_none() {
                    continue;
                }
                *clashes
                    .entry(match keys.contains(&participant.id()) {
                        true => "the level states a box or a visibility for this id",
                        false => "no entry under this id",
                    })
                    .or_default() += 1;
            }
        }
    }
    busiest.sort_by(|a, b| b.0.cmp(&a.0));

    println!("{files} files read, {failed} failed");
    println!("participant instance kinds {instances:?}");
    println!("{undecoded} helpers that did not decode");
    println!("helper kinds {kinds:?}");
    println!("nested instance kinds {nested:?}");
    println!("{placements} placements, flags {flags:?}");
    println!("heights {heights:?}");
    println!(
        "applied placements: {} same position, {} within a metre, {} further; {spread:?}",
        stated[0], stated[1], stated[2]
    );
    for (sheet, [absent, present]) in &ids {
        println!("{sheet}: {present} ids resolve, {absent} do not");
    }
    println!("assets: {} present, {} missing", assets[1], assets[0]);
    for (sheet, id) in unresolved.iter().take(20) {
        println!("  no {sheet} row {id}");
    }
    for asset in missing.iter().take(20) {
        println!("  no file {asset}");
    }

    println!();
    println!("prop participants {props:?}, {propless} naming no nested instance");
    println!(
        "placements overriding a prop: {} state the participant's own transform, {} differ",
        prop_override[0], prop_override[1]
    );
    println!("{nested_moved} nested instances state a transform of their own");
    println!("where a prop override differs: {carrying:?}");
    carrying.clear();
    println!("nested BgPart: {invisible} invisible, {fading} fading, {} with no bounding sphere, shadowing {shadowing:?}", spheres[0]);

    let mut coarse = [0usize, 0];
    let mut drawn = [0usize, 0, 0];
    let mut instances = [0usize, 0, 0];
    let mut empty: BTreeSet<String> = BTreeSet::new();
    for (path, count) in &models {
        let bytes = ironworks.file::<Vec<u8>>(path).ok();
        let held = bytes.clone().and_then(drawable).unwrap_or(0);
        let low = bytes
            .and_then(|bytes| meshes(bytes, Lod::Low))
            .unwrap_or(0);
        coarse[usize::from(low == held)] += 1;
        let slot = usize::from(held > 0);
        drawn[slot] += 1;
        instances[slot] += count;
        if held == 0 {
            empty.insert(path.clone());
        }
    }
    println!(
        "BgPart models: {} of {} paths draw, covering {} of {} participants",
        drawn[1],
        models.len(),
        instances[1],
        instances[0] + instances[1]
    );
    println!(
        "  {} of them hold the same mesh count at the coarsest level, {} fewer",
        coarse[1], coarse[0]
    );
    for path in empty.iter().take(10) {
        println!("  nothing drawn from {path}");
    }

    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut read = [0usize, 0];
    let mut placed = [0usize, 0];
    let mut nothing: BTreeSet<String> = BTreeSet::new();
    for (path, count) in &groups {
        let mut named = BTreeSet::new();
        let mut beside = BTreeMap::new();
        let ok = parts(&ironworks, path, 0, &mut named, &mut beside);
        let mut held = 0usize;
        for model in &named {
            let drawn = *carrying.entry(model.clone()).or_insert_with(|| {
                ironworks
                    .file::<Vec<u8>>(model)
                    .ok()
                    .and_then(drawable)
                    .unwrap_or(0)
            });
            held += usize::from(drawn > 0);
        }
        let slot = usize::from(ok && held > 0);
        read[slot] += 1;
        placed[slot] += count;
        if slot == 0 {
            nothing.insert(format!("{path} {beside:?}"));
            *tally
                .entry(
                    match (ok, beside.contains_key("vfx"), beside.is_empty()) {
                        (false, ..) => "does not parse",
                        (true, true, _) => "places effects and no model",
                        (true, false, true) => "places nothing at all",
                        (true, false, false) => "places lights or sounds and no model",
                    }
                    .to_owned(),
                )
                .or_default() += 1;
        }
    }
    println!();
    println!(
        "{} camera shots, read at their first frame and their middle:",
        cameras.shots
    );
    quantiles("eye -> the nearest character, bound  ", &mut cameras.bound);
    quantiles("eye -> the nearest character, unbound", &mut cameras.loose);
    quantiles("aim angle at that character, bound   ", &mut cameras.aim);
    quantiles("aim angle at that character, unbound ", &mut cameras.aim_loose);
    quantiles("up target -> eye                     ", &mut cameras.span);
    quantiles("angle between up and forward         ", &mut cameras.square);

    println!("prop ids against the level's own lcb/svb keys {clashes:?}");
    println!("busiest cutscenes {:?}", &busiest[..10.min(busiest.len())]);
    println!(
        "SharedGroups: {} of {} paths place a model that draws, covering {} of {} participants",
        read[1],
        groups.len(),
        placed[1],
        placed[0] + placed[1]
    );
    println!("  of the rest {tally:?}");
    for path in nothing.iter().take(6) {
        println!("  nothing drawn from {path}");
    }
}
