//! Whether the two [0,1] scalars in a `Sound` instance's geometry blob (immediately after its
//! constant pair at the offset the position list ends) vary per placement or are fixed per asset,
//! which is what tells a placement-authored volume apart from a property of the sound itself.
//!
//! `zone_sound_attenuation_census`

use std::collections::{BTreeMap, HashSet};

use ironworks::file::layer::{InstanceData, LayerGroup, SoundEffectKind};
use ironworks::file::lvb;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

/// Byte offset of the core attenuation block's first field, past the shape's own position list,
/// keyed by kind.
fn core_offset(kind: SoundEffectKind, _geometry_len: usize) -> Option<usize> {
    use SoundEffectKind::*;
    match kind {
        Point => Some(0x30),
        Line => Some(0x40),
        Surface => Some(0x60),
        PolyLine => Some(0x120),
        Polygon => Some(0x20),
        _ => None,
    }
}

fn f32_at(bytes: &[u8], at: usize) -> Option<f32> {
    bytes.get(at..at + 4)?.try_into().ok().map(f32::from_le_bytes)
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).expect("the path list");

    // path -> (inner set, outer set, vol1 set, vol2 set)
    let mut by_asset: BTreeMap<
        String,
        (HashSet<u32>, HashSet<u32>, HashSet<u32>, HashSet<u32>),
    > = BTreeMap::new();
    let mut by_kind_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut over_one_vol1 = 0;
    let mut over_one_vol2 = 0;
    let mut inner_le_outer = 0;
    let mut total = 0;

    let mut visit = |group: &LayerGroup| {
        for layer in group.layers() {
            for instance in layer.instances() {
                let InstanceData::Sound(sound) = instance.data() else {
                    continue;
                };
                if sound.asset_path().is_empty() {
                    continue;
                }
                let Some(base) = core_offset(sound.kind(), sound.binary().len()) else {
                    continue;
                };
                let bytes = sound.binary();
                let (Some(inner), Some(outer), Some(vol1), Some(vol2)) = (
                    f32_at(bytes, base),
                    f32_at(bytes, base + 4),
                    f32_at(bytes, base + 0x14),
                    f32_at(bytes, base + 0x18),
                ) else {
                    continue;
                };
                total += 1;
                *by_kind_count.entry(format!("{:?}", sound.kind())).or_default() += 1;
                if vol1 > 1.0 {
                    over_one_vol1 += 1;
                }
                if vol2 > 1.0 {
                    over_one_vol2 += 1;
                }
                if inner <= outer {
                    inner_le_outer += 1;
                }
                let entry = by_asset.entry(sound.asset_path().clone()).or_default();
                entry.0.insert(inner.to_bits());
                entry.1.insert(outer.to_bits());
                entry.2.insert(vol1.to_bits());
                entry.3.insert(vol2.to_bits());
            }
        }
    };

    for path in list.lines().filter(|path| path.ends_with(".lvb")) {
        let Ok(level) = ironworks.file::<lvb::LevelFile>(path) else {
            continue;
        };
        let scene = level.scene();
        for group in scene.layer_groups() {
            visit(group);
        }
        for lgb_path in scene.layer_group_paths() {
            if let Ok(file) = ironworks.file::<ironworks::file::lgb::LayerGroupFile>(lgb_path) {
                visit(file.group());
            }
        }
    }

    println!("total sound placements with a resolvable core block: {total}");
    println!("by kind: {by_kind_count:?}");
    println!("vol1 > 1.0: {over_one_vol1}, vol2 > 1.0: {over_one_vol2}");
    println!("inner <= outer: {inner_le_outer} of {total}");

    let paths_with_multiple_placements: Vec<_> =
        by_asset.iter().filter(|(_, (i, _, _, _))| i.len() + 0 >= 0).collect();
    let count_multi = |select: fn(&(HashSet<u32>, HashSet<u32>, HashSet<u32>, HashSet<u32>)) -> usize| {
        by_asset.values().filter(|entry| select(entry) > 1).count()
    };
    let assets_seen_more_than_once = by_asset
        .values()
        .filter(|(i, o, v1, v2)| i.len() + o.len() + v1.len() + v2.len() > 4)
        .count();
    println!("distinct .scd assets used by an emitting Sound: {}", by_asset.len());
    println!(
        "of those, assets whose fields are not all constant across placements: {}",
        assets_seen_more_than_once
    );
    println!("  distinct assets with >1 inner value: {}", count_multi(|(i, _, _, _)| i.len()));
    println!("  distinct assets with >1 outer value: {}", count_multi(|(_, o, _, _)| o.len()));
    println!("  distinct assets with >1 vol1 value: {}", count_multi(|(_, _, v1, _)| v1.len()));
    println!("  distinct assets with >1 vol2 value: {}", count_multi(|(_, _, _, v2)| v2.len()));
    let _ = paths_with_multiple_placements;
}
