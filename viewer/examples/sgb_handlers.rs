//! The animation handler list a scene header keeps in its ninth slot, straight out of the bytes:
//! how many handlers of each kind ship, the shape of each kind's body, and whether the instances a
//! repeating transform names are instances that scene holds.
//!
//! `sgb_handlers <paths file | path> [more paths]`

use std::collections::BTreeMap;

use ironworks::file::layer::InstanceKind;
use ironworks::file::sgb::SharedGroupFile;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// Where the list sits inside the block the ninth slot names.
const LIST: usize = 0x24;

/// The kind whose body is a repeating transform, and the length of one of its three lanes.
const REPEAT: i32 = 5;
const LANE: usize = 36;

#[derive(Default)]
struct Kind {
    records: usize,
    /// The three slots a body ends on, which is what tells the kinds apart.
    bodies: BTreeMap<[i32; 3], usize>,
    first: String,
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<String> = match args.first().map(std::fs::read_to_string) {
        Some(Ok(list)) => list.lines().map(str::to_owned).collect(),
        _ => args,
    };
    let loud = paths.len() <= 16;

    let mut read = 0usize;
    let mut carrying = 0usize;
    let mut anchored = 0usize;
    let mut resolved = 0usize;
    let mut missing = 0usize;
    let mut kinds: BTreeMap<i32, Kind> = BTreeMap::new();
    let mut lanes: Vec<BTreeMap<(bool, u32, u32), usize>> = vec![BTreeMap::new(); 3];
    let mut moved: BTreeMap<String, usize> = BTreeMap::new();
    let mut pairs: BTreeMap<String, usize> = BTreeMap::new();
    let mut named = 0usize;
    let mut contested = 0usize;
    for path in &paths {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path.as_str()) else {
            continue;
        };
        let Some(at) = (0..bytes.len().saturating_sub(4)).find(|at| &bytes[*at..at + 4] == b"SCN1")
        else {
            continue;
        };
        read += 1;
        let word = |at: usize| -> i32 {
            bytes
                .get(at..at + 4)
                .map_or(0, |held| i32::from_le_bytes(held.try_into().unwrap()))
        };
        let float = |at: usize| f32::from_bits(word(at) as u32);
        let reach = |at: usize, offset: i32| -> Option<usize> {
            usize::try_from(at as i64 + i64::from(offset))
                .ok()
                .filter(|held| *held < bytes.len())
        };
        // The older section puts two empty fields ahead of the body.
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
        anchored += usize::from(word(block + LIST) == 8);
        let Some(table) = reach(block + LIST, word(block + LIST)) else {
            continue;
        };
        let count = word(block + LIST + 4);
        if count <= 0 || count > 256 {
            continue;
        }
        carrying += 1;

        // What the scene itself holds, to check the ids a handler names against, and what its own
        // timelines already move, since a handler and a timeline would fight over the same instance.
        let mut present: Vec<(u32, InstanceKind)> = Vec::new();
        let mut played: Vec<u32> = Vec::new();
        if let Ok(scene) = ironworks.file::<SharedGroupFile>(path.as_str()) {
            for group in scene.scene().layer_groups() {
                for layer in group.layers() {
                    present.extend(
                        layer
                            .instances()
                            .iter()
                            .map(|held| (held.id(), held.kind())),
                    );
                }
            }
            for timeline in scene.scene().timelines() {
                if timeline.auto_play() {
                    played.extend(timeline.animated().iter().map(|(_, held)| *held as u32));
                }
            }
        }
        if loud {
            println!("{path}  {count} handlers  instances {present:?}");
        }
        for index in 0..count as usize {
            let Some(record) = reach(table, word(table + index * 4)) else {
                continue;
            };
            if record + 0x2c > bytes.len() {
                continue;
            }
            let kind = kinds.entry(word(record)).or_default();
            kind.records += 1;
            *kind
                .bodies
                .entry([word(record + 32), word(record + 36), word(record + 40)])
                .or_default() += 1;
            if kind.first.is_empty() {
                kind.first = path.clone();
            }
            if word(record) == 2 {
                *pairs
                    .entry(format!("{:.1} {:.1}", float(record + 24), float(record + 28)))
                    .or_default() += 1;
                if present
                    .iter()
                    .any(|(id, _)| *id == word(record + 16) as u32)
                {
                    named += 1;
                }
            }
            if word(record) != REPEAT {
                continue;
            }

            let ids = reach(record, word(record + 16)).unwrap_or_default();
            let driven: Vec<u8> = (0..word(record + 20).clamp(0, 64) as usize)
                .filter_map(|index| bytes.get(ids + index).copied())
                .collect();
            for id in &driven {
                if played.contains(&u32::from(*id)) {
                    contested += 1;
                }
                match present.iter().find(|(held, _)| *held == u32::from(*id)) {
                    Some((_, kind)) => {
                        resolved += 1;
                        *moved.entry(format!("{kind:?}")).or_default() += 1;
                    }
                    None => missing += 1,
                }
            }
            if loud {
                println!("   repeats over {driven:?}");
            }
            for lane in 0..3 {
                let Some(timer) = reach(record, word(record + 32 + lane * 4)) else {
                    continue;
                };
                if timer + LANE > bytes.len() {
                    continue;
                }
                let active = word(timer) != 0;
                if loud {
                    println!(
                        "      {} {} ({:.3}, {:.3}, {:.3}, {:.3}) over {} after {}  curve {} wrap {}",
                        ["shift", "turn ", "size "][lane as usize],
                        match active {
                            true => "on ",
                            false => "off",
                        },
                        float(timer + 4),
                        float(timer + 8),
                        float(timer + 12),
                        float(timer + 16),
                        word(timer + 20),
                        word(timer + 24),
                        word(timer + 28),
                        word(timer + 32),
                    );
                }
                if active {
                    *lanes[lane as usize]
                        .entry((
                            // A whole turn is the case a lane can wrap rather than swing back.
                            [4, 8, 12]
                                .iter()
                                .any(|at| (float(timer + at).abs() - std::f32::consts::TAU).abs() < 1e-3),
                            word(timer + 28) as u32,
                            word(timer + 32) as u32,
                        ))
                        .or_default() += 1;
                }
            }
        }
    }

    println!("{read} scenes read of {} paths", paths.len());
    println!("{carrying} carry a handler, {anchored} lay the list out at the ninth slot + 0x24");
    println!("{resolved} of the instances a repeating transform names are the scene's, {missing} are not");
    println!("   what a repeating transform moves {moved:?}");
    println!("   {contested} of them a playing timeline of the same scene also moves");
    let mut sorted: Vec<_> = pairs.iter().collect();
    sorted.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    println!(
        "   kind 2 names an instance of its own scene {named} times, pairs {:?}",
        &sorted[..sorted.len().min(12)]
    );
    for (kind, held) in &kinds {
        let bodies: Vec<String> = held
            .bodies
            .iter()
            .filter(|(_, count)| **count > 3)
            .map(|(body, count)| format!("{body:?}x{count}"))
            .collect();
        println!(
            "   kind {kind}: {} records, first {}  bodies {}",
            held.records,
            held.first,
            bodies.join(" ")
        );
    }
    for (lane, held) in lanes.iter().enumerate() {
        let shown: Vec<String> = held
            .iter()
            .map(|((turn, curve, wrap), count)| {
                format!("{}curve {curve} wrap {wrap}: {count}", match turn {
                    true => "whole turn, ",
                    false => "",
                })
            })
            .collect();
        println!("   {}  {}", ["shift", "turn", "size"][lane], shown.join("  "));
    }
}
