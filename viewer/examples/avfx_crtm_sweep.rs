//! Scratch tool: correlate a particle-item's `CrTm` with how many bursts its emitter's own
//! `CrI`/`EdTm` mechanics (constant curves only) produce, to see whether `CrTm` marks one-shot vs
//! continuous emitters.

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

    // bursts-bucket -> (CrTm value -> count)
    let mut table: BTreeMap<i64, BTreeMap<i32, u64>> = BTreeMap::new();
    let mut crtm_hist: BTreeMap<i32, u64> = BTreeMap::new();

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
                let props = emitter.properties();
                let (Some(interval), Some(count)) =
                    (constant(props, "CrI"), constant(props, "CrC"))
                else {
                    continue;
                };
                if count <= 0.0 || interval < 1.0 {
                    continue;
                }
                let life = find(props, "Life")
                    .and_then(|b| b.find("Val"))
                    .and_then(Block::f32)
                    .filter(|&v| v >= 0.0);
                let span = life.map_or(edtm, |life| edtm.min(life as i32));
                let interval = interval.max(1.0);
                let bursts = (span as f32 / interval).floor() as i64 + 1;
                let bucket = bursts.min(4);

                for particle_item in emitter.particles() {
                    if let Some(crtm) = particle_item.find("CrTm").and_then(Block::i32) {
                        *crtm_hist.entry(crtm).or_default() += 1;
                        *table.entry(bucket).or_default().entry(crtm).or_default() += 1;
                    }
                }
            }
        }
    }

    println!("CrTm overall histogram (top 20):");
    let mut sorted: Vec<_> = crtm_hist.iter().collect();
    sorted.sort_by_key(|&(_, c)| std::cmp::Reverse(*c));
    for (v, c) in sorted.iter().take(20) {
        println!("  CrTm={v:6}  x{c}");
    }

    println!("\nCrTm distribution by burst-count bucket:");
    for (bucket, dist) in &table {
        let label = if *bucket >= 4 {
            "4+".to_owned()
        } else {
            bucket.to_string()
        };
        let mut d: Vec<_> = dist.iter().collect();
        d.sort_by_key(|&(_, c)| std::cmp::Reverse(*c));
        let total: u64 = dist.values().sum();
        print!("  bursts={label:>2} (n={total:>7}): ");
        for (v, c) in d.iter().take(6) {
            print!("CrTm={v}:{c} ");
        }
        println!();
    }
}
