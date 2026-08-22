//! Every light a zone places, as the file states it, beside the box its `.lcb` clips it against.
//!
//! `zone_lights <bg/.../level>` prints one row per light; a place after it keeps only the lights
//! within a metre of there. `zone_lights scan` walks every territory instead and tallies the fields
//! a light's reach is worked out from.

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::sync::Arc;

use glam::{Mat3, Mat4, Quat, Vec3};
use ironworks::file::layer::{Glow, InstanceData, LayerGroup, LightKind, SceneGlow, Transform};
use ironworks::file::{File, lcb, lgb::LayerGroupFile, lvb, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn rotation(angles: [f32; 3]) -> Mat3 {
    Mat3::from_rotation_z(angles[2])
        * Mat3::from_rotation_y(angles[1])
        * Mat3::from_rotation_x(angles[0])
}

fn matrix(transform: Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(transform.scale()),
        Quat::from_mat3(&rotation(transform.rotation())),
        Vec3::from_array(transform.translation()),
    )
}

fn keyed(key: (u32, [u8; 4]), depth: u8, id: u32) -> (u32, [u8; 4]) {
    if depth == 0 {
        return (id, [0; 4]);
    }
    let mut held = key.1;
    if let Some(slot) = held.get_mut(usize::from(depth) - 1) {
        *slot = id as u8;
    }
    (key.0, held)
}

struct Placed {
    at: Vec3,
    scale: f32,
    kind: LightKind,
    range: f32,
    attenuation: f32,
    cone: f32,
    spot: f32,
    color: Vec3,
    intensity: f32,
    clip: Option<(Vec3, Vec3)>,
    glow: Option<Glow>,
}

type Clips = HashMap<(u32, [u8; 4]), (Vec3, Vec3)>;

fn walk(
    ironworks: &Arc<Ironworks>,
    clips: &Clips,
    groups: &[LayerGroup],
    glows: &[SceneGlow],
    transform: Mat4,
    key: (u32, [u8; 4]),
    depth: u8,
    out: &mut Vec<Placed>,
) {
    for instance in groups
        .iter()
        .flat_map(LayerGroup::layers)
        .flat_map(|layer| layer.instances())
    {
        let here = transform * matrix(instance.transform());
        let key = keyed(key, depth, instance.id());
        match instance.data() {
            InstanceData::SharedGroup(shared) if depth < 4 && !shared.asset_path().is_empty() => {
                let Ok(bytes) = ironworks.file::<Vec<u8>>(shared.asset_path()) else {
                    continue;
                };
                let Ok(held) = SharedGroupFile::read(Cursor::new(bytes)) else {
                    continue;
                };
                walk(
                    ironworks,
                    clips,
                    held.scene().layer_groups(),
                    SceneGlow::of(held.scene()),
                    here,
                    key,
                    depth + 1,
                    out,
                );
            }
            InstanceData::Light(light) => {
                let held = light.colour();
                let color = Vec3::new(
                    f32::from(held.red()),
                    f32::from(held.green()),
                    f32::from(held.blue()),
                ) / 255.0;
                out.push(Placed {
                    at: here.transform_point3(Vec3::ZERO),
                    scale: here.to_scale_rotation_translation().0.abs().max_element(),
                    kind: light.kind(),
                    range: light.range(),
                    attenuation: light.attenuation(),
                    cone: light.attenuation_cone_coefficient(),
                    spot: light.spot_angle(),
                    color,
                    intensity: held.intensity(),
                    clip: clips.get(&key).copied(),
                    glow: glows
                        .iter()
                        .find(|held| held.instances().contains(&instance.id()))
                        .map(SceneGlow::light)
                        .filter(|held| held.active() && held.tints()),
                });
            }
            _ => {}
        }
    }
}

fn lights(ironworks: &Arc<Ironworks>, level: &str) -> Vec<Placed> {
    let stem = level.rsplit('/').nth(1).unwrap_or_default();
    let scene = ironworks
        .file::<lvb::LevelFile>(&format!("{level}/{stem}.lvb"))
        .ok();
    let glows = scene
        .as_ref()
        .map(lvb::LevelFile::scene)
        .map_or(&[][..], SceneGlow::of);
    let mut clips = Clips::new();
    if let Ok(bytes) = ironworks.file::<Vec<u8>>(&format!("{level}/{stem}.lcb"))
        && let Ok(held) = lcb::ClipBoxes::read(Cursor::new(bytes))
    {
        for entry in held.groups().iter().flat_map(lcb::Group::entries) {
            clips.insert(
                (entry.instance(), entry.members()),
                (Vec3::from_array(entry.min()), Vec3::from_array(entry.max())),
            );
        }
    }
    let mut out = Vec::new();
    for name in ["bg", "planlive", "planmap", "planevent"] {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(&format!("{level}/{name}.lgb")) else {
            continue;
        };
        let Ok(group) = LayerGroupFile::read(Cursor::new(bytes)) else {
            continue;
        };
        walk(
            ironworks,
            &clips,
            std::slice::from_ref(group.group()),
            glows,
            Mat4::IDENTITY,
            (0, [0; 4]),
            0,
            &mut out,
        );
    }
    out
}

fn scan(ironworks: &Arc<Ironworks>) {
    let excel = ironworks::excel::Excel::new(ironworks.clone());
    let sheet = excel.sheet("TerritoryType").expect("the sheet");
    let mut seen: Vec<String> = Vec::new();
    let mut attenuation: BTreeMap<u32, usize> = BTreeMap::new();
    let mut range: BTreeMap<u32, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let (mut total, mut unclipped, mut zones) = (0usize, 0usize, 0usize);
    for row in 0..2000u32 {
        let Ok(held) = sheet.row(row) else { continue };
        let Ok(ironworks::excel::Field::String(bg)) = held.field(1) else {
            continue;
        };
        let bg = bg.to_string();
        if bg.is_empty() || seen.contains(&bg) {
            continue;
        }
        seen.push(bg.clone());
        if ironworks
            .file::<lvb::LevelFile>(&format!("bg/{bg}.lvb"))
            .is_err()
        {
            continue;
        }
        let Some((directory, _)) = bg.rsplit_once('/') else {
            continue;
        };
        let held = lights(ironworks, &format!("bg/{directory}"));
        if held.is_empty() {
            continue;
        }
        zones += 1;
        total += held.len();
        for light in &held {
            unclipped += usize::from(light.clip.is_none());
            *attenuation.entry(light.attenuation.to_bits()).or_default() += 1;
            *range.entry(light.range.to_bits()).or_default() += 1;
            *kinds.entry(format!("{:?}", light.kind)).or_default() += 1;
        }
    }
    println!("{zones} zones, {total} lights, {unclipped} with no clip box");
    for (name, held) in [("attenuation", &attenuation), ("range", &range)] {
        let mut rows: Vec<(f32, usize)> = held
            .iter()
            .map(|(bits, count)| (f32::from_bits(*bits), *count))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        let tally: Vec<String> = rows
            .iter()
            .take(12)
            .map(|(value, count)| format!("{value}x{count}"))
            .collect();
        println!("{name}: {} distinct, {}", rows.len(), tally.join(" "));
    }
    println!("kinds: {kinds:?}");
}

fn main() {
    let ironworks: Arc<Ironworks> = Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let mut args = std::env::args().skip(1);
    let level = args.next().expect("a level directory or `scan`");
    if level == "scan" {
        scan(&ironworks);
        return;
    }
    let held: Vec<f32> = args.filter_map(|one| one.parse().ok()).collect();
    let at = (held.len() == 3).then(|| Vec3::new(held[0], held[1], held[2]));

    for light in lights(&ironworks, level.trim_end_matches('/')) {
        if at.is_some_and(|want| (light.at - want).length() > 1.0) {
            continue;
        }
        let color = light.color * light.intensity;
        let glow = light.glow.map_or_else(
            || "-".to_owned(),
            |lane| {
                let end = |held: ironworks::file::layer::Colour| {
                    format!(
                        "({},{},{})x{}",
                        held.red(),
                        held.green(),
                        held.blue(),
                        held.intensity()
                    )
                };
                format!("{}..{} over {}", end(lane.from()), end(lane.to()), lane.period())
            },
        );
        let clip = light.clip.map_or_else(
            || "-".to_owned(),
            |(min, max)| {
                format!(
                    "[{:.3},{:.3},{:.3}..{:.3},{:.3},{:.3}]",
                    min.x, min.y, min.z, max.x, max.y, max.z
                )
            },
        );
        println!(
            "({:9.3},{:9.3},{:9.3}) {:?} scale={:<7.4} range={:<7.4} atten={:<5.2} cone={:<7.3} \
             spot={:<7.3} rgb=({:.5},{:.5},{:.5}) i={:.5} peak={:.5} box={clip} glow={glow}",
            light.at.x,
            light.at.y,
            light.at.z,
            light.kind,
            light.scale,
            light.range,
            light.attenuation,
            light.cone,
            light.spot,
            color.x,
            color.y,
            color.z,
            light.intensity,
            color.max_element(),
        );
    }
}
