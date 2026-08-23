//! Scratch tool: for every timeline item across the corpus, replay the emitter's own burst
//! mechanics (constant CrI/CrC/Life only) over its StTm..EdTm span and histogram how many bursts
//! come out, to see whether repeat-bursting over a bounded span is common or a rare edge case.

use std::collections::BTreeMap;
use std::io::Cursor;

use ironworks::{
    Ironworks,
    file::File,
    file::avfx::{Avfx, Block},
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn find<'a>(blocks: &'a [Block], name: &str) -> Option<&'a Block> {
    blocks.iter().find(|b| b.name() == name)
}

/// A constant-valued curve's single key, or `None` if it is animated or absent.
fn constant(blocks: &[Block], name: &str) -> Option<f32> {
    let curve = find(blocks, name)?;
    let keys = curve.find("Keys")?.keys()?;
    match keys {
        [only] => Some(only.value()),
        _ => None,
    }
}

fn main() {
    let sqpack = std::env::var("SQPACK").unwrap_or_else(|_| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    let list_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/asriel/Code/ironworks-formats/paths.txt".to_owned());
    let paths: Vec<String> = std::fs::read_to_string(&list_path)
        .unwrap()
        .lines()
        .filter(|line| line.ends_with(".avfx"))
        .map(str::to_owned)
        .collect();

    let mut burst_hist: BTreeMap<i64, u64> = BTreeMap::new();
    let mut skipped_animated = 0u64;
    let mut skipped_unbounded = 0u64;
    let mut considered = 0u64;

    for (i, path) in paths.iter().enumerate() {
        if i % 5000 == 0 {
            eprintln!("{i}/{}", paths.len());
        }
        let bytes: Vec<u8> = match ironworks.file(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let file = match Avfx::read(Cursor::new(bytes)) {
            Ok(f) => f,
            Err(_) => continue,
        };

        for timeline in file.timelines() {
            for item in timeline.items() {
                let blocks = item.blocks();
                let Some(edtm) = blocks
                    .iter()
                    .find(|b| b.name() == "EdTm")
                    .and_then(Block::i32)
                else {
                    skipped_unbounded += 1;
                    continue;
                };
                if edtm <= 0 {
                    skipped_unbounded += 1;
                    continue;
                }
                let Some(em_no) = blocks
                    .iter()
                    .find(|b| b.name() == "EmNo")
                    .and_then(Block::i32)
                else {
                    continue;
                };
                let Some(emitter) = usize::try_from(em_no)
                    .ok()
                    .and_then(|i| file.emitters().get(i))
                else {
                    continue;
                };
                let props = emitter.properties();
                let (Some(interval), Some(count)) =
                    (constant(props, "CrI"), constant(props, "CrC"))
                else {
                    skipped_animated += 1;
                    continue;
                };
                if count <= 0.0 || interval < 1.0 {
                    continue;
                }
                let life = find(props, "Life")
                    .and_then(|b| b.find("Val"))
                    .and_then(Block::f32)
                    .filter(|&v| v >= 0.0);
                let span = life.map_or(edtm, |life| (edtm).min(life as i32));
                let interval = interval.max(1.0);
                let bursts = (span as f32 / interval).floor() as i64 + 1;
                *burst_hist.entry(bursts.min(10)).or_default() += 1;
                considered += 1;
            }
        }
    }

    println!(
        "considered={considered} skipped_animated={skipped_animated} skipped_unbounded={skipped_unbounded}"
    );
    for (bursts, count) in &burst_hist {
        let label = if *bursts >= 10 {
            "10+".to_owned()
        } else {
            bursts.to_string()
        };
        println!("  {label:>3} bursts: x{count}");
    }
}
