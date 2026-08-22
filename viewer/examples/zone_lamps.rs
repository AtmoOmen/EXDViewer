//! Every light a zone places, walked the way the scene walks it, against the box its `.lcb` clips
//! each to and the environment volumes it stands in.
//!
//! `zone_lamps <path.lvb> [x y z [radius]]`

use std::collections::BTreeMap;

use ironworks::file::layer::{InstanceData, LightKind};
use ironworks::file::{lcb, lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

use glam::{Mat4, Quat, Vec3};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;
const REACH: f32 = 6.0;

type Pack = Ironworks<SqPack<Install>>;

struct Lamp {
    kind: LightKind,
    falloff: f32,
    at: Vec3,
    forward: Vec3,
    color: Vec3,
    intensity: f32,
    range: f32,
    cone: f32,
    key: (u32, [u8; 4]),
    under: String,
}

struct Space {
    at: Vec3,
    shape: i32,
    range: f32,
    bound: u32,
    path: String,
}

fn matrix(placed: ironworks::file::layer::Transform) -> Mat4 {
    let rotation = placed.rotation();
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(placed.scale()),
        Quat::from_euler(glam::EulerRot::ZYX, rotation[2], rotation[1], rotation[0]),
        Vec3::from_array(placed.translation()),
    )
}

fn reach(key: (u32, [u8; 4]), depth: usize, id: u32) -> (u32, [u8; 4]) {
    if depth == 0 {
        return (id, [0; 4]);
    }
    let mut held = key.1;
    if let Some(slot) = held.get_mut(depth - 1) {
        *slot = id as u8;
    }
    (key.0, held)
}

fn walk(
    ironworks: &Pack,
    instances: &[ironworks::file::layer::Instance],
    transform: Mat4,
    key: (u32, [u8; 4]),
    depth: usize,
    under: &str,
    lamps: &mut Vec<Lamp>,
    spaces: &mut Vec<Space>,
    located: &mut BTreeMap<u32, String>,
) {
    for instance in instances {
        let here = transform * matrix(instance.transform());
        let center = here.transform_point3(Vec3::ZERO);
        let key = reach(key, depth, instance.id());
        match instance.data() {
            InstanceData::Light(light) => {
                let held = light.colour();
                lamps.push(Lamp {
                    kind: light.kind(),
                    falloff: light.attenuation(),
                    at: center,
                    forward: here.transform_vector3(Vec3::Z).normalize_or_zero(),
                    color: Vec3::new(
                        f32::from(held.red()),
                        f32::from(held.green()),
                        f32::from(held.blue()),
                    ) / 255.0,
                    intensity: held.intensity(),
                    range: light.range(),
                    cone: (light.spot_angle() + light.attenuation_cone_coefficient()) * 0.5,
                    key,
                    under: under.to_owned(),
                });
            }
            InstanceData::EnvSpace(space) => spaces.push(Space {
                at: center,
                shape: space.shape() as i32,
                range: space.effective_range(),
                bound: space.bound_instance_id(),
                path: space.asset_path().to_owned(),
            }),
            InstanceData::EnvLocation(env) => {
                located.insert(instance.id(), env.ambient_light_asset_path().to_owned());
            }
            InstanceData::SharedGroup(held) if depth < DEPTH && !held.asset_path().is_empty() => {
                let Ok(file) = ironworks.file::<SharedGroupFile>(held.asset_path()) else {
                    continue;
                };
                for group in file.scene().layer_groups() {
                    for layer in group.layers() {
                        walk(
                            ironworks,
                            layer.instances(),
                            here,
                            key,
                            depth + 1,
                            held.asset_path(),
                            lamps,
                            spaces,
                            located,
                        );
                    }
                }
            }
            _ => (),
        }
    }
}

fn name(kind: LightKind) -> &'static str {
    match kind {
        LightKind::None => "none",
        LightKind::World => "world",
        LightKind::Point => "point",
        LightKind::Spot => "spot",
        LightKind::Flat => "plane",
        LightKind::Line => "line",
        LightKind::Specular => "specular",
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let zone = args.next().expect("a level path");
    let number = |held: Option<String>| held.and_then(|one| one.parse::<f32>().ok());
    let eye = match (number(args.next()), number(args.next()), number(args.next())) {
        (Some(x), Some(y), Some(z)) => Some(Vec3::new(x, y, z)),
        _ => None,
    };
    let radius = number(args.next()).unwrap_or(40.0);

    let level: LevelFile = ironworks.file(&zone).unwrap();
    let mut lamps = Vec::new();
    let mut spaces = Vec::new();
    let mut located = BTreeMap::new();
    for path in level.scene().layer_group_paths() {
        let Ok(file) = ironworks.file::<LayerGroupFile>(path) else {
            continue;
        };
        for layer in file.group().layers() {
            walk(
                &ironworks,
                layer.instances(),
                Mat4::IDENTITY,
                (0, [0; 4]),
                0,
                path,
                &mut lamps,
                &mut spaces,
                &mut located,
            );
        }
    }

    let stem = zone.rsplit('/').next().unwrap_or(&zone).replace(".lvb", "");
    let root = zone.rsplit_once('/').map(|(head, _)| head).unwrap_or("");
    let mut clips: BTreeMap<(u32, [u8; 4]), (Vec3, Vec3)> = BTreeMap::new();
    if let Ok(held) = ironworks.file::<lcb::ClipBoxes>(&format!("{root}/{stem}.lcb")) {
        for group in held.groups() {
            for entry in group.entries() {
                clips.insert(
                    (entry.instance(), entry.members()),
                    (Vec3::from_array(entry.min()), Vec3::from_array(entry.max())),
                );
            }
        }
    }
    println!("{} lights, {} clip entries", lamps.len(), clips.len());

    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for lamp in &lamps {
        *kinds.entry(name(lamp.kind)).or_default() += 1;
    }
    println!("kinds: {kinds:?}");

    println!("\nby height, 8 unit bands:");
    let mut bands: BTreeMap<i32, BTreeMap<&str, usize>> = BTreeMap::new();
    for lamp in &lamps {
        *bands
            .entry((lamp.at.y / 8.0).floor() as i32 * 8)
            .or_default()
            .entry(name(lamp.kind))
            .or_default() += 1;
    }
    for (floor, held) in &bands {
        let total: usize = held.values().sum();
        println!("  y {floor:>5}..{:<5} {total:>5}  {held:?}", floor + 8);
    }

    if let Some(eye) = eye {
        println!("\nlights whose clip box comes within {radius} of {eye:?}:");
        let mut near: Vec<(f32, &Lamp)> = lamps
            .iter()
            .map(|lamp| {
                let (min, max) = clips
                    .get(&lamp.key)
                    .copied()
                    .unwrap_or((Vec3::splat(-REACH), Vec3::splat(REACH)));
                let reach = min.abs().max(max.abs()).max_element();
                (((lamp.at - eye).length() - reach).max(0.0), lamp)
            })
            .filter(|(span, _)| *span <= radius)
            .collect();
        near.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, lamp) in &near {
            *counts.entry(name(lamp.kind)).or_default() += 1;
        }
        println!("  {} of them: {counts:?}", near.len());
        for (span, lamp) in near.iter().take(60) {
            let box_ = clips.get(&lamp.key).copied();
            println!(
                "  {span:>7.2}  {:<8} at ({:>8.1},{:>7.1},{:>8.1})  rgb ({:.2},{:.2},{:.2}) x{:<6.2} range {:<8.2} cone {:>7.2} deg  fwd ({:>5.2},{:>5.2},{:>5.2})  box {}  under {}",
                name(lamp.kind),
                lamp.at.x,
                lamp.at.y,
                lamp.at.z,
                lamp.color.x,
                lamp.color.y,
                lamp.color.z,
                lamp.intensity,
                lamp.range,
                lamp.cone,
                lamp.forward.x,
                lamp.forward.y,
                lamp.forward.z,
                match box_ {
                    Some((min, max)) => format!(
                        "[{:.1},{:.1},{:.1}]..[{:.1},{:.1},{:.1}]",
                        min.x, min.y, min.z, max.x, max.y, max.z
                    ),
                    None => "stand-in".to_owned(),
                },
                lamp.under,
            );
        }
    }

    println!("\nevery light, one line each:");
    for lamp in &lamps {
        let (min, max) = clips
            .get(&lamp.key)
            .copied()
            .unwrap_or((Vec3::splat(-REACH), Vec3::splat(REACH)));
        println!(
            "LIGHT {} {:.4} {:.4} {:.4} {:.5} {:.5} {:.5} {:.4} {:.4} {:.1} {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} {}",
            name(lamp.kind),
            lamp.at.x, lamp.at.y, lamp.at.z,
            lamp.color.x, lamp.color.y, lamp.color.z,
            lamp.intensity, lamp.range, lamp.falloff, lamp.cone,
            min.x, min.y, min.z, max.x, max.y, max.z,
            clips.contains_key(&lamp.key),
        );
    }

    println!("\n{} env spaces, {} env locations", spaces.len(), located.len());
    for space in &spaces {
        let bound = located.get(&space.bound);
        println!(
            "  at ({:>8.1},{:>7.1},{:>8.1})  shape {}  range {:>8.2}  bound {}  {}  amb {}",
            space.at.x,
            space.at.y,
            space.at.z,
            space.shape,
            space.range,
            space.bound,
            space.path,
            bound.map_or("-", String::as_str),
        );
    }
}
