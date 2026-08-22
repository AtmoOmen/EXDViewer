//! What a zone's scenery states about casting, which is a mode per instance rather than per model.
//!
//! `zone_castoff bg/ex3/01_nvt_n4/twn/n4t1/level/n4t1.lvb`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::layer::{Instance, InstanceData, ShadowMode};
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;

type Pack = Ironworks<SqPack<Install>>;

fn walk(
    pack: &Pack,
    instances: &[Instance],
    out: &mut Vec<(String, ShadowMode, ShadowMode)>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) {
    for instance in instances {
        match instance.data() {
            InstanceData::BgPart(held) if !held.asset_path().is_empty() => out.push((
                held.asset_path().clone(),
                held.world_light_shadow_mode(),
                held.object_light_shadow_mode(),
            )),
            InstanceData::SharedGroup(held)
                if depth < DEPTH
                    && !held.asset_path().is_empty()
                    && seen.insert(held.asset_path().clone()) =>
            {
                let Ok(file) = pack.file::<SharedGroupFile>(held.asset_path()) else {
                    continue;
                };
                for group in file.scene().layer_groups() {
                    for layer in group.layers() {
                        walk(pack, layer.instances(), out, seen, depth + 1);
                    }
                }
            }
            _ => (),
        }
    }
}

fn main() {
    let pack = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for zone in std::env::args().skip(1) {
        let level: LevelFile = pack.file(&zone).expect("a level");
        let mut placed = Vec::new();
        let mut seen = BTreeSet::new();
        for path in level.scene().layer_group_paths() {
            let Ok(file) = pack.file::<LayerGroupFile>(path) else {
                continue;
            };
            for layer in file.group().layers() {
                walk(&pack, layer.instances(), &mut placed, &mut seen, 0);
            }
        }

        let mut tally: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut off: BTreeMap<&str, usize> = BTreeMap::new();
        for (path, world, object) in &placed {
            *tally
                .entry((format!("{world:?}"), format!("{object:?}")))
                .or_default() += 1;
            if *world == ShadowMode::ForceOff {
                *off.entry(path).or_default() += 1;
            }
        }
        println!("== {zone}: {} placements", placed.len());
        for ((world, object), count) in &tally {
            println!("   {count:>6}  sun {world}, lamps {object}");
        }
        for (path, count) in &off {
            println!("   x{count:<5} {path}");
        }
    }
}
