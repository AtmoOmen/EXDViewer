//! What a node the crate does not read lays out: its header, the stride of its table, and where the
//! table's offsets reach. Run over `CTEX` it sees only the ones whose table of runs does not read.
//!
//! `cutb_ctex <cutb path> [magic]` dumps one file's nodes; `cutb_ctex <paths file>` counts.

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

fn text(bytes: &[u8], at: usize) -> String {
    let rest = bytes.get(at..).unwrap_or_default();
    let end = rest.iter().position(|byte| *byte == 0).unwrap_or(0);
    String::from_utf8_lossy(&rest[..end.min(64)]).into_owned()
}

fn dump(body: &[u8]) {
    println!("== {} bytes", body.len());
    for at in (0..body.len().min(0x100)).step_by(4) {
        let value = word(body, at);
        let reach = at as i64 + value as i64;
        let mut note = String::new();
        if value > 0 && (value as usize) < body.len() {
            note = format!("  ->{:?}", text(body, value as usize));
        }
        if reach > 0 && (reach as usize) < body.len() {
            note.push_str(&format!("  +>{:?}", text(body, reach as usize)));
        }
        println!("  {at:#06x} {value:>10} {value:#010x}{note}");
    }
    let mut at = 0;
    while at < body.len() {
        let run = text(body, at);
        if run.len() >= 2 && run.chars().all(|held| held.is_ascii_graphic() || held == ' ') {
            println!("  string {at:#06x} {run:?}");
            at += run.len();
        }
        at += 1;
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let held = args.next().expect("a cutb path or a paths file");
    let wanted = args.next().unwrap_or_else(|| "CTEX".to_owned());

    if let Ok(list) = std::fs::read_to_string(&held) {
        // How many of a node's leading words look like an offset that lands on a string, and what
        // the table stride has to be for its entries to tile the node.
        let mut heads: BTreeMap<[u32; 6], usize> = BTreeMap::new();
        let mut strides: BTreeMap<usize, usize> = BTreeMap::new();
        let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
        let mut nodes = 0usize;
        let mut shapes: BTreeMap<&str, usize> = BTreeMap::new();
        let mut floats = 0usize;
        let mut outside = 0usize;
        for path in list.lines() {
            let Ok(file) = ironworks.file::<Cutscene>(path) else {
                continue;
            };
            for node in file.nodes() {
                let Node::Unknown(unknown) = node else {
                    continue;
                };
                if unknown.magic() != wanted.as_bytes() {
                    continue;
                }
                let body = unknown.body();
                nodes += 1;
                *sizes.entry(body.len()).or_default() += 1;
                if body.len() <= 128 {
                    *heads
                        .entry([
                            word(body, 0),
                            word(body, 4),
                            word(body, 8),
                            word(body, 12),
                            word(body, 16),
                            word(body, 20),
                        ])
                        .or_default() += 1;
                }
                // A table of 24-byte records at 0x28 whose offsets tile the node: the first one
                // lands where the table ends, they rise, and the last block runs to the end.
                if body.len() <= 128 {
                    continue;
                }
                let count = word(body, 0x14) as usize;
                let table = 0x28;
                let verdict = match table + count * 24 <= body.len() {
                    false => "the table leaves the node",
                    true => {
                        let offsets: Vec<usize> = (0..count)
                            .map(|index| word(body, table + index * 24 + 4) as usize)
                            .collect();
                        let rising = offsets.windows(2).all(|held| held[0] < held[1]);
                        let last = offsets.last().copied().unwrap_or(0);
                        match (
                            offsets.first() == Some(&(count * 24)),
                            rising,
                            table + last < body.len(),
                            offsets.iter().all(|held| held % 4 == 0),
                        ) {
                            (true, true, true, true) => "tiles",
                            (false, _, _, _) => "the first offset is not the table end",
                            (_, false, _, _) => "the offsets do not rise",
                            (_, _, false, _) => "the last offset leaves the node",
                            _ => "an offset is not a multiple of four",
                        }
                    }
                };
                *strides.entry(verdict.len()).or_default() += 1;
                *shapes.entry(verdict).or_default() += 1;
                if verdict != "tiles" && strides.len() < 400 {
                    println!(
                        "   {verdict}: {path} {} bytes head {:?} count {count} first {}",
                        body.len(),
                        (0..8).map(|held| word(body, held * 4)).collect::<Vec<_>>(),
                        word(body, table + 4)
                    );
                }
                // Every float a block holds, so the payload can be described.
                let end = body.len();
                let start = table + word(body, table + 4) as usize;
                for at in (start..end).step_by(4) {
                    let held = f32::from_bits(word(body, at));
                    floats += 1;
                    if !(0.0..=1.0).contains(&held) {
                        outside += 1;
                    }
                }
            }
        }
        println!("{nodes} {wanted} nodes, {} distinct sizes", sizes.len());
        let mut shown: Vec<(&usize, &usize)> = sizes.iter().collect();
        shown.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        println!("commonest sizes: {:?}", &shown[..shown.len().min(10)]);
        let mut common: Vec<([u32; 6], usize)> = heads.into_iter().collect();
        common.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (head, count) in common.iter().take(12) {
            println!("   head {head:?} x{count}");
        }
        let _ = strides;
        println!("payload nodes by how their table reads: {shapes:?}");
        println!("{floats} floats in the blocks, {outside} outside nought to one");
        return;
    }

    let file: Cutscene = ironworks.file(&held).expect("the cutscene");
    for node in file.nodes() {
        match node {
            Node::Unknown(unknown) if unknown.magic() == wanted.as_bytes() => dump(unknown.body()),
            _ => (),
        }
    }
}
