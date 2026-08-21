//! Kind-6 animation handler records, as bytes and as the instances they name, so the light lanes
//! can be decoded off the corpus.
//!
//! `sgb_lights <paths file | path> [more paths]`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::layer::{InstanceData, InstanceKind};
use ironworks::file::tmb;
use ironworks::file::sgb::SharedGroupFile;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const LIST: usize = 0x24;
const LIGHT: i32 = 6;
const LANE: usize = 60;

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
    let loud = paths.len() <= 16;

    let mut records = 0usize;
    let mut slots: Vec<BTreeMap<u32, usize>> = vec![BTreeMap::new(); 10];
    let mut fields: Vec<Vec<BTreeMap<u32, usize>>> = vec![vec![BTreeMap::new(); 15]; 2];
    let mut named: BTreeMap<String, usize> = BTreeMap::new();
    let mut missing = 0usize;
    let mut contested = 0usize;
    let mut ids_per = BTreeMap::new();
    let mut states: BTreeMap<String, usize> = BTreeMap::new();
    let mut fought: BTreeMap<String, usize> = BTreeMap::new();

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
        let raw = |at: usize| word(at) as u32;
        let float = |at: usize| f32::from_bits(raw(at));
        let reach = |at: usize, offset: i32| -> Option<usize> {
            usize::try_from(at as i64 + i64::from(offset))
                .ok()
                .filter(|held| *held < bytes.len())
        };
        let body = match (word(at + 8), word(at + 12)) {
            (0, 0) => at + 16,
            _ => at + 8,
        };
        let Some(block) = reach(body, word(body + 8 * 4)) else {
            continue;
        };
        if block + LIST + 8 > bytes.len() {
            continue;
        }
        let Some(table) = reach(block + LIST, word(block + LIST)) else {
            continue;
        };
        let count = word(block + LIST + 4);
        if count <= 0 || count > 256 {
            continue;
        }

        let mut present: Vec<(u32, InstanceKind, String, [f32; 4])> = Vec::new();
        let mut played: Vec<u32> = Vec::new();
        let mut runs: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
        if let Ok(scene) = ironworks.file::<SharedGroupFile>(path.as_str()) {
            for group in scene.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        let held = match instance.data() {
                            InstanceData::Light(light) => {
                                let colour = light.colour();
                                [
                                    f32::from(colour.red()),
                                    f32::from(colour.green()),
                                    f32::from(colour.blue()),
                                    colour.intensity(),
                                ]
                            }
                            _ => [0.0; 4],
                        };
                        let asset = match instance.data() {
                            InstanceData::BgPart(part) => part.asset_path().clone(),
                            InstanceData::Vfx(vfx) => vfx.asset_path().clone(),
                            _ => String::new(),
                        };
                        present.push((
                            instance.id(),
                            instance.kind(),
                            format!("{}~{asset}", instance.name()),
                            held,
                        ));
                    }
                }
            }
            for timeline in scene.scene().timelines() {
                if !timeline.auto_play() {
                    continue;
                }
                played.extend(timeline.animated().iter().map(|(_, held)| *held as u32));
                // What the timeline runs against each instance it drives, so a colour a handler
                // states can be told from a place one does.
                let held = timeline.timeline();
                for (actor, instance) in timeline.animated() {
                    let Some(tracks) = held.items().iter().find_map(|item| match item {
                        tmb::Item::Actor(held) if i32::from(held.time()) == *actor => {
                            Some(held.tracks())
                        }
                        _ => None,
                    }) else {
                        continue;
                    };
                    for track in tracks {
                        let Some(commands) = held.items().iter().find_map(|item| match item {
                            tmb::Item::Track(held) if held.id() == *track => Some(held.commands()),
                            _ => None,
                        }) else {
                            continue;
                        };
                        for command in commands {
                            let Some(tmb::Item::Command(found)) =
                                held.items().iter().find(|item| {
                                    matches!(item, tmb::Item::Command(held) if held.id() == *command)
                                })
                            else {
                                continue;
                            };
                            let magic = format!("{:?}", found.kind());
                            let magic = magic.split('(').next().unwrap_or_default().to_owned();
                            runs.entry(*instance as u32).or_default().insert(magic);
                        }
                    }
                }
            }
            for group in scene.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        if let InstanceData::SharedGroup(held) = instance.data() {
                            *states
                                .entry(format!("{:?}", held.initial_colour_state()))
                                .or_default() += 1;
                        }
                    }
                }
            }
        }

        for index in 0..count as usize {
            let Some(record) = reach(table, word(table + index * 4)) else {
                continue;
            };
            if record + 160 > bytes.len() || word(record) != LIGHT {
                continue;
            }
            records += 1;
            for slot in 0..10 {
                *slots[slot].entry(raw(record + slot * 4)).or_default() += 1;
            }
            let ids = reach(record, word(record + 16)).unwrap_or_default();
            let driven: Vec<u8> = (0..word(record + 20).clamp(0, 64) as usize)
                .filter_map(|index| bytes.get(ids + index).copied())
                .collect();
            *ids_per.entry(driven.len()).or_insert(0usize) += 1;
            for id in &driven {
                let kind = present
                    .iter()
                    .find(|(held, ..)| *held == u32::from(*id))
                    .map(|(_, kind, ..)| format!("{kind:?}"));
                if played.contains(&u32::from(*id)) {
                    contested += 1;
                    // Only the lane that answers to the instance's own kind, and only where its
                    // second byte says it states a colour at all.
                    let lane = record + 40 + usize::from(kind.as_deref() == Some("Light")) * LANE;
                    let states = (raw(lane) >> 8) & 0xff != 0;
                    for magic in runs.get(&u32::from(*id)).into_iter().flatten() {
                        if !magic.starts_with("C112") && !magic.starts_with("C113") {
                            continue;
                        }
                        *fought
                            .entry(format!(
                                "{} {magic} against a lane that states a colour: {states}",
                                kind.clone().unwrap_or_default()
                            ))
                            .or_default() += 1;
                    }
                }
                match kind {
                    Some(kind) => *named.entry(kind).or_default() += 1,
                    None => missing += 1,
                }
            }
            let lit: Vec<String> = driven
                .iter()
                .map(|id| {
                    let held = present.iter().find(|(held, ..)| *held == u32::from(*id));
                    match held {
                        Some((_, kind, name, colour)) => format!(
                            "{id}:{kind:?}:{name}:{colour:?}:{}",
                            played.contains(&u32::from(*id))
                        ),
                        None => format!("{id}:?"),
                    }
                })
                .collect();
            if loud {
                println!("H {}", hex(&bytes[record..record + 160]));
            }
            println!("R|{path}|{}|{}", word(record + 20), lit.join(","));
            for lane in 0..2 {
                let at = record + 40 + lane * LANE;
                for slot in 0..15 {
                    *fields[lane][slot].entry(raw(at + slot * 4)).or_default() += 1;
                }
                let held: Vec<String> = (0..15)
                    .map(|slot| match slot {
                        1 | 3 => format!("{:#010x}", raw(at + slot * 4)),
                        0 | 5 | 8 | 9 | 10 | 13 | 14 => format!("{}", word(at + slot * 4)),
                        _ => format!("{:.4}", float(at + slot * 4)),
                    })
                    .collect();
                println!("L|{lane}|{}", held.join("|"));
            }
        }
    }

    println!("records {records}, ids per record {ids_per:?}");
    println!("named {named:?}, missing {missing}, contested {contested}");
    println!("what an autoplaying timeline runs on a contested instance {fought:?}");
    println!("initial_colour_state over the shared groups these scenes hold {states:?}");
    let show = |what: &str, held: &BTreeMap<u32, usize>| {
        let mut sorted: Vec<_> = held.iter().collect();
        sorted.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let top: Vec<String> = sorted
            .iter()
            .take(8)
            .map(|(value, count)| {
                format!("{:#010x}/{:.4}={count}", value, f32::from_bits(**value))
            })
            .collect();
        println!("{what} distinct {} : {}", held.len(), top.join("  "));
    };
    for slot in 0..10 {
        show(&format!("head {slot}"), &slots[slot]);
    }
    for lane in 0..2 {
        for slot in 0..15 {
            show(&format!("lane {lane} slot {slot}"), &fields[lane][slot]);
        }
    }
}
