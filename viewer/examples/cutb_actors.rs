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
use ironworks::file::pbd::PreBoneDeformer;
use ironworks::file::sgb::SharedGroupFile;
use ironworks::file::tmb::{Channel, CommandKind, Curves, Item};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

/// How deep a shared group is followed into another, matching the scene view's own cap.
const DEPTH: u8 = 8;

/// Where `ENpcBase` writes what the creator would have picked, and the places in it this reads.
const CUSTOMIZE: usize = 52;
const RACE: usize = 0;
const GENDER: usize = 1;
const BODY: usize = 2;
const TRIBE: usize = 4;
const FACE: usize = 5;
const HAIRSTYLE: usize = 6;
const TAIL: usize = 22;
const CHILD_BODY: u8 = 4;
/// The rest of `ENpcBase`: the ten model quads, the body it states apart from the creator's
/// numbering, and the row it is dressed out of.
const MODELS: usize = 35;
const MODEL_CHARA: usize = 46;
const NPC_EQUIP: usize = 47;
/// `NpcEquip`'s own ten, and `ModelChara`'s numbered body.
const EQUIP_MODELS: usize = 2;
const CHARA_MODEL: usize = 3;
const CHARA_KIND: usize = 5;
const CHARA_BASE: usize = 6;
/// `BNpcBase`'s links to all three.
const BNPC_CHARA: usize = 4;
const BNPC_CUSTOMIZE: usize = 5;
const BNPC_EQUIP: usize = 6;

/// The `ENpcBase` a participant copying a live character falls back to, out of `sub_141B26310`: a
/// stand-in for a party member, and the one body a `StableChocobo` always draws.
const PARTY_STAND_IN: u32 = 1_034_882;
const STABLED_CHOCOBO: u32 = 1_006_001;

/// The body a clan is grown on, as `viewer/src/character` resolves one.
const BUILT_ON: [u16; 16] = [1, 3, 5, 5, 11, 11, 7, 7, 9, 9, 13, 13, 15, 15, 17, 17];
const ADULT: u16 = 1;
const CHILD: u16 = 4;
const BODY_SET: u16 = 1;
const SUFFIXES: [&str; 10] = [
    "met", "top", "glv", "dwn", "sho", "ear", "nek", "wrs", "ril", "rir",
];

fn resolve(tribe: u8, female: bool, child: bool) -> u16 {
    let body = BUILT_ON
        .get(usize::from(tribe.max(1)) - 1)
        .copied()
        .unwrap_or(1);
    (body + u16::from(female)) * 100 + if child { CHILD } else { ADULT }
}

/// Which schema field each of a sheet's columns is: EXDSchema lists them in offset order, and
/// ironworks indexes them in the exh's own.
fn ordered(sheet: &ironworks::excel::Sheet<&str>) -> Vec<usize> {
    let held = sheet.columns().unwrap_or_default();
    let mut at: Vec<usize> = (0..held.len()).collect();
    at.sort_by_key(|c| (held[*c].offset(), format!("{:?}", held[*c].kind())));
    at
}

/// What a character participant is built out of.
enum Cast {
    /// A human, by the body its clan grows on and what it wears.
    Human {
        code: u16,
        face: u16,
        hair: u16,
        tail: u16,
        outfit: [Option<(u16, bool)>; 10],
    },
    /// A body of its own, drawn from every model under one directory.
    Beast { under: String },
}

/// Every model path this cast would draw, against a directory index of the whole install.
fn drawn_from(cast: &Cast, under: &BTreeMap<String, Vec<String>>, built_on: &BTreeMap<u16, u16>) -> Vec<String> {
    let lineage = |code: u16| std::iter::successors(Some(code), |code| built_on.get(code).copied());
    match cast {
        Cast::Beast { under: held } => under.get(held).cloned().unwrap_or_default(),
        Cast::Human {
            code,
            face,
            hair,
            tail,
            outfit,
        } => {
            let mut found = Vec::new();
            // A set the code no longer carries draws its lowest, the way the creator picks one.
            let numbered = |kind: &str, wanted: u16| -> Vec<String> {
                let letter = kind.as_bytes()[0] as char;
                let root = format!("chara/human/c{code:04}/obj/{kind}/{letter}");
                let held = format!("{root}{wanted:04}/model/");
                if let Some(parts) = under.get(&held) {
                    return parts.clone();
                }
                under
                    .range(root.clone()..)
                    .take_while(|(path, _)| path.starts_with(&root))
                    .map(|(_, parts)| parts.clone())
                    .next()
                    .unwrap_or_default()
            };
            found.extend(numbered("face", *face));
            found.extend(numbered("hair", *hair));
            for kind in ["tail", "zear"] {
                found.extend(numbered(kind, *tail));
            }
            let body = lineage(*code)
                .filter_map(|code| {
                    under.get(&format!(
                        "chara/human/c{code:04}/obj/body/b{BODY_SET:04}/model/"
                    ))
                })
                .next()
                .cloned()
                .unwrap_or_default();
            for (slot, suffix) in SUFFIXES.into_iter().enumerate() {
                let worn = outfit[slot].and_then(|(set, adornment)| {
                    let (kind, letter) = match adornment {
                        true => ("accessory", 'a'),
                        false => ("equipment", 'e'),
                    };
                    let held = under.get(&format!("chara/{kind}/{letter}{set:04}/model/"))?;
                    lineage(*code).find_map(|code| {
                        let path =
                            format!("chara/{kind}/{letter}{set:04}/model/c{code:04}{letter}{set:04}_{suffix}.mdl");
                        held.contains(&path).then_some(path)
                    })
                });
                match worn {
                    Some(path) => found.push(path),
                    None => found.extend(
                        body.iter()
                            .find(|path| path.ends_with(&format!("_{suffix}.mdl")))
                            .cloned(),
                    ),
                }
            }
            found
        }
    }
}

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
        let Some(held) = placements
            .get(&bindings[slot])
            .or_else(|| placements.get(&bindings[slot + 2]))
        else {
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
    // Every model, under the directory holding it: what a character is worn out of is a directory
    // rather than a name, and nothing in an install answers that without a path list.
    let mut under: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in paths.lines().filter(|path| path.ends_with(".mdl")) {
        let Some(at) = path.rfind('/') else { continue };
        under
            .entry(path[..=at].to_owned())
            .or_default()
            .push(path.to_owned());
    }
    for parts in under.values_mut() {
        parts.sort();
    }
    let built_on: BTreeMap<u16, u16> = ironworks
        .file::<Vec<u8>>("chara/xls/boneDeformer/human.pbd")
        .ok()
        .and_then(|bytes| PreBoneDeformer::read(std::io::Cursor::new(bytes)).ok())
        .map(|file| {
            file.deformers()
                .filter_map(|deformer| {
                    Some((deformer.id(), deformer.node().parent()?.deformer().id()))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut casts: BTreeMap<(bool, u32), usize> = BTreeMap::new();
    let mut cast_kinds: BTreeMap<&str, usize> = BTreeMap::new();

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
                // What each kind that draws a character resolves to with no live one on hand,
                // which is the branch `sub_141B26310` takes wherever the roster it indexes is
                // empty and the record does not force an id of its own.
                let stands = match helper.kind() {
                    HelperKind::EventNpc => Some((false, helper.base_id())),
                    HelperKind::BattleNpc => Some((true, helper.base_id())),
                    HelperKind::Player => Some((false, helper.base_id())),
                    HelperKind::PartyMember | HelperKind::PartyMemberAlt | HelperKind::Unknown82 => {
                        Some((false, match helper.forces_base_id() {
                            true => helper.base_id(),
                            false => PARTY_STAND_IN,
                        }))
                    }
                    HelperKind::StableChocobo => Some((false, STABLED_CHOCOBO)),
                    _ => None,
                };
                if let Some(key) = stands.filter(|(_, id)| *id != 0) {
                    *casts.entry(key).or_default() += 1;
                    *cast_kinds
                        .entry(match helper.kind() {
                            HelperKind::EventNpc => "EventNpc",
                            HelperKind::BattleNpc => "BattleNpc",
                            HelperKind::Player => "Player",
                            HelperKind::StableChocobo => "StableChocobo",
                            _ => "PartyMember",
                        })
                        .or_default() += 1;
                }
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
    println!("character participants by kind {cast_kinds:?}");
    let bases = excel.sheet("ENpcBase").unwrap();
    let battle = excel.sheet("BNpcBase").unwrap();
    let customizes = excel.sheet("BNpcCustomize").unwrap();
    let charas = excel.sheet("ModelChara").unwrap();
    let equips = excel.sheet("NpcEquip").unwrap();
    let (base_at, battle_at, customize_at, chara_at, equip_at) = (
        ordered(&bases),
        ordered(&battle),
        ordered(&customizes),
        ordered(&charas),
        ordered(&equips),
    );
    let mut why: BTreeMap<&str, usize> = BTreeMap::new();
    let mut stood = 0usize;
    let mut standing: BTreeMap<&str, usize> = BTreeMap::new();
    let mut geometry = [0usize; 2];
    let mut pieces: BTreeMap<usize, usize> = BTreeMap::new();
    let mut absent: BTreeSet<String> = BTreeSet::new();
    for ((battle_npc, id), seen) in &casts {
        stood += seen;
        let byte = |row: &ironworks::excel::Row, at: &[usize], field: usize| -> u8 {
            row.field(at[field])
                .ok()
                .and_then(|held| held.into_u8().ok())
                .unwrap_or(0)
        };
        let link = |row: &ironworks::excel::Row, at: &[usize], field: usize| -> u32 {
            row.field(at[field])
                .ok()
                .and_then(|held| held.into_u16().ok())
                .map_or(0, u32::from)
        };
        let quads = |row: &ironworks::excel::Row, at: &[usize], first: usize| -> [Option<(u16, bool)>; 10] {
            let mut worn = [None; 10];
            for slot in 0..10 {
                let held = row
                    .field(at[first + slot])
                    .ok()
                    .and_then(|held| held.into_u32().ok())
                    .unwrap_or(0);
                worn[slot] = (held != 0 && held != u32::MAX).then_some((held as u16, slot >= 5));
            }
            worn
        };
        let (customize, own, chara, equip) = match battle_npc {
            false => {
                let Ok(row) = bases.row(*id) else {
                    *why.entry("no row under this id").or_default() += seen;
                    continue;
                };
                let chara = link(&row, &base_at, MODEL_CHARA);
                let equip = link(&row, &base_at, NPC_EQUIP);
                let outfit = quads(&row, &base_at, MODELS);
                (Some((row, base_at.clone(), CUSTOMIZE)), outfit, chara, equip)
            }
            true => {
                let Ok(row) = battle.row(*id) else {
                    *why.entry("no row under this id").or_default() += seen;
                    continue;
                };
                let chara = link(&row, &battle_at, BNPC_CHARA);
                let equip = link(&row, &battle_at, BNPC_EQUIP);
                let held = link(&row, &battle_at, BNPC_CUSTOMIZE);
                let customize = customizes
                    .row(held)
                    .ok()
                    .map(|held| (held, customize_at.clone(), 0));
                (customize, [None; 10], chara, equip)
            }
        };
        let human = customize
            .as_ref()
            .filter(|(held, at, first)| byte(held, at, first + RACE) != 0);
        let cast = match human {
            Some((held, held_at, first)) => {
                let tribe = byte(held, held_at, first + TRIBE);
                let female = byte(held, held_at, first + GENDER) != 0;
                let child = byte(held, held_at, first + BODY) == CHILD_BODY;
                // Its own quads unless it states none at all, in which case the `NpcEquip` it
                // names is what dresses it.
                let mut outfit = own;
                if outfit.iter().all(Option::is_none) && equip != 0
                    && let Ok(held) = equips.row(equip)
                {
                    outfit = quads(&held, &equip_at, EQUIP_MODELS);
                }
                Cast::Human {
                    code: resolve(tribe, female, child),
                    face: u16::from(byte(held, held_at, first + FACE)),
                    hair: u16::from(byte(held, held_at, first + HAIRSTYLE)),
                    tail: u16::from(byte(held, held_at, first + TAIL)),
                    outfit,
                }
            }
            None => {
                let held = charas.row(chara).ok().filter(|_| chara != 0);
                let Some(held) = held else {
                    *why.entry("no race and no ModelChara").or_default() += seen;
                    continue;
                };
                let (model, kind, base) = (
                    held.field(chara_at[CHARA_MODEL]).unwrap().into_u16().unwrap(),
                    byte(&held, &chara_at, CHARA_KIND),
                    byte(&held, &chara_at, CHARA_BASE),
                );
                match kind {
                    3 => Cast::Beast {
                        under: format!("chara/monster/m{model:04}/obj/body/b{base:04}/model/"),
                    },
                    2 => Cast::Beast {
                        under: format!("chara/demihuman/d{model:04}/obj/equipment/e{base:04}/model/"),
                    },
                    _ => {
                        *why.entry("ModelChara names a body this does not resolve")
                            .or_default() += seen;
                        continue;
                    }
                }
            }
        };
        *standing
            .entry(match cast {
                Cast::Human { .. } => "human",
                Cast::Beast { .. } => "monster or demihuman",
            })
            .or_default() += seen;
        let held = drawn_from(&cast, &under, &built_on);
        let drawn = held
            .iter()
            .filter(|path| {
                ironworks
                    .file::<Vec<u8>>(path)
                    .ok()
                    .and_then(drawable)
                    .is_some_and(|meshes| meshes > 0)
            })
            .count();
        geometry[usize::from(drawn > 0)] += seen;
        *pieces.entry(drawn.min(20)).or_default() += seen;
        if drawn == 0 {
            *why.entry(match held.is_empty() {
                true => "nothing on disk under what it names",
                false => "every model it names is empty",
            })
            .or_default() += seen;
            if let Cast::Beast { under } = &cast {
                absent.insert(under.clone());
            }
        }
    }
    println!(
        "{} of {stood} character participants draw geometry, over {} distinct ids",
        geometry[1],
        casts.len()
    );
    println!("  what they stand as {standing:?}");
    println!("  models drawn per participant {pieces:?}");
    println!("  why the rest do not {why:?}");
    for path in absent.iter().take(10) {
        println!("  nothing under {path}");
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
