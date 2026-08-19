//! The timeline commands the shared groups hold that no reference implementation names, grouped by
//! magic so a body's fixed and varying fields separate.
//!
//! `sgb_commands <paths file> [magic]`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::layer::InstanceData;
use ironworks::file::{sgb::SharedGroupFile, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

#[derive(Default)]
struct Shape {
    bodies: usize,
    lengths: BTreeSet<usize>,
    /// What each dword of the body is ever set to.
    fields: Vec<BTreeSet<u32>>,
    files: BTreeSet<String>,
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a paths file");
    let wanted = std::env::args().nth(2).unwrap_or_default();
    let paths = std::fs::read_to_string(list).expect("the paths file");

    // A path list does not name every shared group, so the ones a group holds are followed too.
    let mut every: BTreeSet<String> = paths.lines().map(str::to_owned).collect();
    let mut wave: Vec<String> = every.iter().cloned().collect();
    while !wave.is_empty() {
        let mut next = Vec::new();
        for path in &wave {
            let Ok(file) = ironworks.file::<SharedGroupFile>(path) else {
                continue;
            };
            for group in file.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        if let InstanceData::SharedGroup(child) = instance.data()
                            && !child.asset_path().is_empty()
                            && every.insert(child.asset_path().clone())
                        {
                            next.push(child.asset_path().clone());
                        }
                    }
                }
            }
        }
        wave = next;
    }
    println!("{} shared groups", every.len());

    let mut held: BTreeMap<String, Shape> = BTreeMap::new();
    let mut curves: BTreeMap<String, usize> = BTreeMap::new();
    let mut moved: BTreeMap<(bool, bool), usize> = BTreeMap::new();
    let mut asleep: Vec<String> = Vec::new();
    let mut wide = 0usize;
    for path in &every {
        let Ok(file) = ironworks.file::<SharedGroupFile>(path.as_str()) else {
            continue;
        };
        for timeline in file.scene().timelines() {
            let items = timeline.timeline().items();
            wide += usize::from(timeline.timeline().layout() == tmb::Layout::Wide);
            let driven = items
                .iter()
                .filter(|item| match item {
                    tmb::Item::Command(command) => {
                        matches!(command.kind(), tmb::CommandKind::C013(_))
                    }
                    _ => false,
                })
                .count();
            if driven > 0 {
                *moved
                    .entry((timeline.auto_play(), timeline.looping()))
                    .or_default() += driven;
                if !timeline.auto_play() && asleep.len() < 12 {
                    asleep.push(format!(
                        "{path} sub {} {:?} drives {:?}",
                        timeline.sub_id(),
                        timeline.kind(),
                        timeline.animated()
                    ));
                }
            }
            let sets: BTreeSet<i32> = items
                .iter()
                .filter_map(|item| match item {
                    tmb::Item::Curves(curves) => Some(i32::from(curves.id())),
                    _ => None,
                })
                .collect();
            for item in items {
                let tmb::Item::Command(command) = item else {
                    continue;
                };
                let tmb::CommandKind::Unknown { magic, body } = command.kind() else {
                    continue;
                };
                let magic = String::from_utf8_lossy(magic).into_owned();
                if !wanted.is_empty() && magic != wanted {
                    continue;
                }
                let shape = held.entry(magic.clone()).or_default();
                shape.bodies += 1;
                shape.lengths.insert(body.len());
                shape.files.insert(path.clone());
                for (at, dword) in body.chunks_exact(4).enumerate() {
                    let value = u32::from_le_bytes(dword.try_into().unwrap());
                    if shape.fields.len() <= at {
                        shape.fields.resize(at + 1, BTreeSet::new());
                    }
                    shape.fields[at].insert(value);
                    if sets.contains(&(value as i32)) {
                        *curves.entry(format!("{magic} field {at}")).or_default() += 1;
                    }
                }
                if !wanted.is_empty() {
                    println!("{path}  sub {}  {body:?}", timeline.sub_id());
                }
            }
        }
    }

    for (magic, shape) in &held {
        println!(
            "{magic}: {} bodies in {} files, lengths {:?}",
            shape.bodies,
            shape.files.len(),
            shape.lengths
        );
        for (at, values) in shape.fields.iter().enumerate() {
            let shown: Vec<String> = values.iter().take(10).map(|held| held.to_string()).collect();
            println!(
                "   field {at}: {} distinct  {}{}",
                values.len(),
                shown.join(" "),
                if values.len() > 10 { " ..." } else { "" }
            );
        }
    }
    println!("{wide} timelines of the wide layout");
    for ((auto, looping), count) in &moved {
        println!("C013: {count} in timelines auto {auto} loop {looping}");
    }
    for line in &asleep {
        println!("   asleep {line}");
    }
    for (field, count) in &curves {
        println!("{field} names a curve set in its own timeline {count} times");
    }
}
