//! Every animation handler record a scene holds, as raw bytes, beside the instances and timelines
//! of the same scene, so a kind whose body is not read yet can be decoded off the corpus.
//!
//! `sgb_dump <paths file | path> [more paths]`

use ironworks::file::layer::{InstanceData, InstanceKind};
use ironworks::file::sgb::SharedGroupFile;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const LIST: usize = 0x24;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|held| format!("{held:02x}")).collect()
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<String> = match args.first().map(std::fs::read_to_string) {
        Some(Ok(list)) => list.lines().map(str::to_owned).collect(),
        _ => args,
    };

    for path in &paths {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path.as_str()) else {
            continue;
        };
        let Some(at) = (0..bytes.len().saturating_sub(4)).find(|at| &bytes[*at..at + 4] == b"SCN1")
        else {
            continue;
        };
        let word = |at: usize| -> i32 {
            bytes
                .get(at..at + 4)
                .map_or(0, |held| i32::from_le_bytes(held.try_into().unwrap()))
        };
        let reach = |at: usize, offset: i32| -> Option<usize> {
            usize::try_from(at as i64 + i64::from(offset))
                .ok()
                .filter(|held| *held < bytes.len())
        };
        let body = match (word(at + 8), word(at + 12)) {
            (0, 0) => at + 16,
            _ => at + 8,
        };
        println!("F {path} {}", bytes.len());

        if let Ok(scene) = ironworks.file::<SharedGroupFile>(path.as_str()) {
            for group in scene.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        let asset = match instance.data() {
                            InstanceData::BgPart(held) => held.asset_path().clone(),
                            InstanceData::SharedGroup(held) => held.asset_path().clone(),
                            InstanceData::Vfx(held) => held.asset_path().clone(),
                            _ => String::new(),
                        };
                        let held = instance.transform();
                        println!(
                            "I|{}|{:?}|{}|{}|{:?}|{:?}|{:?}",
                            instance.id(),
                            instance.kind(),
                            instance.name(),
                            asset,
                            held.translation(),
                            held.rotation(),
                            held.scale(),
                        );
                    }
                }
            }
            for spin in scene.scene().spins() {
                println!(
                    "S|{}|{}|{}",
                    spin.instance(),
                    spin.axis(),
                    spin.period(),
                );
            }
            for timeline in scene.scene().timelines() {
                println!(
                    "T|{}|{}|{}|{}|{:?}",
                    timeline.sub_id(),
                    timeline.kind(),
                    timeline.auto_play(),
                    timeline.looping(),
                    timeline.animated(),
                );
            }
        }

        let Some(block) = reach(body, word(body + 8 * 4)) else {
            continue;
        };
        if block + LIST + 8 > bytes.len() {
            continue;
        }
        // The sixteen bytes the block opens with, ahead of the list, which nothing has identified.
        println!("B {} {}", block, hex(&bytes[block..(block + LIST).min(bytes.len())]));
        let Some(table) = reach(block + LIST, word(block + LIST)) else {
            continue;
        };
        let count = word(block + LIST + 4);
        if count <= 0 || count > 512 {
            continue;
        }
        let starts: Vec<usize> = (0..count as usize)
            .filter_map(|index| reach(table, word(table + index * 4)))
            .collect();
        let mut sorted = starts.clone();
        sorted.push(bytes.len());
        sorted.sort_unstable();
        for (index, &record) in starts.iter().enumerate() {
            let end = sorted
                .iter()
                .find(|held| **held > record)
                .copied()
                .unwrap_or(bytes.len());
            let shown = (end - record).min(0x200);
            println!(
                "R {index} {} {record} {} {}",
                word(record),
                end - record,
                hex(&bytes[record..record + shown])
            );
            // Whatever each dword could be reaching at, so a body nothing reads yet is visible.
            for slot in 0..(shown / 4).min(20) {
                let held = word(record + slot * 4);
                let Some(target) = reach(record, held) else {
                    continue;
                };
                if held <= 0 || held as usize >= end - record {
                    continue;
                }
                let room = (bytes.len() - target).min(0x60);
                println!("   O {slot} {held} {}", hex(&bytes[target..target + room]));
            }
        }
        let _ = InstanceKind::None;
    }
}
