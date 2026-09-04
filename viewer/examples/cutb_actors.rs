//! What the `CTAL` participants of every shipping cutscene decode to, and whether what they name
//! resolves.
//!
//! `cutb_actors <paths file>`

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ironworks::excel::{Excel, Language};
use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::layer::{HelperKind, InstanceData, InstanceKind};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

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

    for path in paths.lines().filter(|path| path.ends_with(".cutb")) {
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
    }

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
}
