//! What a `CTPA` record's middle dword indexes, and the shape of a `CTCB`.
//!
//! `cutb_groups <cutb paths file>`

use std::collections::BTreeMap;

use ironworks::file::cutb::{Cutscene, Node};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn word(bytes: &[u8], at: usize) -> u32 {
    match bytes.get(at..at + 4) {
        Some(held) => u32::from_le_bytes(held.try_into().unwrap()),
        None => 0,
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(std::env::args().nth(1).expect("a paths file")).unwrap();

    let (mut records, mut multiple, mut within_group, mut within_node) = (0usize, 0usize, 0usize, 0usize);
    let mut middles: BTreeMap<u32, usize> = BTreeMap::new();
    let mut firsts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut lasts: BTreeMap<u32, usize> = BTreeMap::new();
    let mut group_sizes: BTreeMap<usize, usize> = BTreeMap::new();

    let mut cbs = 0usize;
    let mut cb_sized = 0usize;
    let mut cb_stride: BTreeMap<usize, usize> = BTreeMap::new();
    let mut cb_fields: Vec<BTreeMap<u32, usize>> = vec![BTreeMap::new(); 6];
    let mut cb_records = 0usize;
    let (mut track_nodes, mut track_runs, mut values, mut outside) = (0usize, 0usize, 0usize, 0usize);
    let mut lanes: BTreeMap<u32, usize> = BTreeMap::new();
    let mut lengths: BTreeMap<usize, usize> = BTreeMap::new();

    for path in list.lines() {
        let Ok(file) = ironworks.file::<Cutscene>(path) else {
            continue;
        };
        for node in file.nodes() {
            match node {
                Node::Groups(groups) => {
                    let held: usize = groups.iter().map(|group| group.records().len()).sum();
                    multiple += usize::from(groups.len() > 1);
                    for group in groups {
                        group_sizes.entry(group.records().len()).and_modify(|count| *count += 1).or_insert(1);
                        for record in group.records() {
                            records += 1;
                            let first = word(record, 0);
                            let middle = word(record, 4);
                            let last = word(record, 8);
                            *middles.entry(middle % 12).or_default() += 1;
                            *firsts.entry(first).or_default() += 1;
                            *lasts.entry(last).or_default() += 1;
                            if (middle as usize) / 12 < group.records().len() {
                                within_group += 1;
                            }
                            if (middle as usize) / 12 < held {
                                within_node += 1;
                            }
                        }
                    }
                }
                Node::Unknown(unknown) if unknown.magic() == *b"CTCB" => {
                    let body = unknown.body();
                    cbs += 1;
                    cb_sized += usize::from(word(body, 0) as usize == body.len());
                    for stride in [16usize, 20, 24, 28, 32] {
                        if body.len().saturating_sub(8) % stride == 0 {
                            *cb_stride.entry(stride).or_default() += 1;
                        }
                    }
                    let mut at = 8;
                    while at + 24 <= body.len() {
                        cb_records += 1;
                        for (field, held) in cb_fields.iter_mut().enumerate() {
                            *held.entry(word(body, at + field * 4)).or_default() += 1;
                        }
                        at += 24;
                    }
                }
                Node::Tracks(tracks) => {
                    track_nodes += 1;
                    track_runs += tracks.len();
                    for track in tracks {
                        *lanes.entry(track.lane()).or_default() += 1;
                        *lengths.entry(track.values().len()).or_default() += 1;
                        values += track.values().len();
                        outside += track
                            .values()
                            .iter()
                            .filter(|held| !(0.0..=1.0).contains(*held))
                            .count();
                    }
                }
                _ => (),
            }
        }
    }
    println!(
        "CTEX: {track_nodes} nodes read as runs, {track_runs} runs, {values} values, {outside} outside nought to one"
    );
    println!("   lanes: {lanes:?}");
    let mut shown: Vec<(usize, usize)> = lengths.into_iter().collect();
    shown.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("   run lengths: {} distinct, commonest {:?}", shown.len(), &shown[..shown.len().min(6)]);
    println!("CTPA: {records} records, {multiple} nodes holding more than one group");
    println!("   middle dword modulo twelve: {middles:?}");
    println!("   middle/12 inside its own group: {within_group}; inside the node's records: {within_node}");
    println!("   first dword: {} distinct, commonest {:?}", firsts.len(), firsts.iter().max_by_key(|(_, count)| **count));
    println!("   last dword: {} distinct, commonest {:?}", lasts.len(), lasts.iter().max_by_key(|(_, count)| **count));
    let mut sizes: Vec<(usize, usize)> = group_sizes.into_iter().collect();
    sizes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("   group sizes: {:?}", &sizes[..sizes.len().min(8)]);
    println!("CTCB: {cbs} nodes, {cb_sized} whose first dword is their own length, {cb_records} records at 24");
    println!("   body past eight divides by: {cb_stride:?}");
    for (field, held) in cb_fields.iter().enumerate() {
        let mut shown: Vec<(&u32, &usize)> = held.iter().collect();
        shown.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        println!("   field {field}: {} distinct  {:?}", held.len(), &shown[..shown.len().min(6)]);
    }
}
