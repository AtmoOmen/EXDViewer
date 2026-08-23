//! Scratch tool: correlate a timeline's own `LpSt`/`LpEd` loop range with how many bursts the
//! items running under it produce, to check whether the timeline's loop bounds gate whether an
//! emitter's `CrI` interval repeats at all.

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

    // (loop configured?) -> (bursts bucket -> count)
    let mut table: BTreeMap<bool, BTreeMap<i64, u64>> = BTreeMap::new();

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
            let props = timeline.properties();
            let lpst = find(props, "LpSt").and_then(Block::i32).unwrap_or(0);
            let lped = find(props, "LpEd").and_then(Block::i32).unwrap_or(0);
            let looped = lped > lpst;

            for item in timeline.items() {
                let blocks = item.blocks();
                let Some(edtm) = blocks
                    .iter()
                    .find(|b| b.name() == "EdTm")
                    .and_then(Block::i32)
                else {
                    continue;
                };
                if edtm <= 0 {
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
                let eprops = emitter.properties();
                let (Some(interval), Some(count)) =
                    (constant(eprops, "CrI"), constant(eprops, "CrC"))
                else {
                    continue;
                };
                if count <= 0.0 || interval < 1.0 {
                    continue;
                }
                let life = find(eprops, "Life")
                    .and_then(|b| b.find("Val"))
                    .and_then(Block::f32)
                    .filter(|&v| v >= 0.0);
                let span = life.map_or(edtm, |life| edtm.min(life as i32));
                let interval = interval.max(1.0);
                let bursts = (span as f32 / interval).floor() as i64 + 1;
                *table
                    .entry(looped)
                    .or_default()
                    .entry(bursts.min(4))
                    .or_default() += 1;
            }
        }
    }

    for (looped, dist) in &table {
        let total: u64 = dist.values().sum();
        println!("loop configured (LpEd>LpSt) = {looped}  (n={total})");
        for (bursts, count) in dist {
            let label = if *bursts >= 4 {
                "4+".to_owned()
            } else {
                bursts.to_string()
            };
            let pct = 100.0 * *count as f64 / total as f64;
            println!("   {label:>3} bursts: x{count:7}  ({pct:.1}%)");
        }
    }
}
