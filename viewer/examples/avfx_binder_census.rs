//! For every timeline item across the corpus, whether `BdNo` is set, and (for the first files
//! that set it) what the binder block it names actually holds.

use std::collections::BTreeMap;
use std::io::Cursor;

use ironworks::{
    Ironworks,
    file::File,
    file::avfx::{Avfx, Block},
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn dump_block(block: &Block, depth: usize) {
    let indent = "  ".repeat(depth);
    match block.payload() {
        ironworks::file::avfx::Payload::Blocks(children) => {
            println!("{indent}{} ({} children)", block.name(), children.len());
            for child in children {
                dump_block(child, depth + 1);
            }
        }
        ironworks::file::avfx::Payload::Bytes(bytes) => {
            println!(
                "{indent}{} [{} bytes] i32={:?} f32={:?} text={:?}",
                block.name(),
                bytes.len(),
                block.i32(),
                block.f32(),
                block.text(),
            );
        }
        ironworks::file::avfx::Payload::Keys(keys) => {
            println!("{indent}{} ({} keys)", block.name(), keys.len());
        }
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

    let mut bdno_hist: BTreeMap<i32, u64> = BTreeMap::new();
    let mut files_with_binders = 0u64;
    let mut files_read = 0u64;
    let mut dumped = 0;

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
        files_read += 1;
        if !file.binders().is_empty() {
            files_with_binders += 1;
        }

        let mut file_has_bdno = false;
        for timeline in file.timelines() {
            for item in timeline.items() {
                let blocks = item.blocks();
                let bd_no = blocks
                    .iter()
                    .find(|b| b.name() == "BdNo")
                    .and_then(Block::i32)
                    .unwrap_or(-1);
                *bdno_hist.entry(bd_no).or_default() += 1;
                if bd_no >= 0 {
                    file_has_bdno = true;
                }
            }
        }

        if file_has_bdno && dumped < 5 {
            dumped += 1;
            println!("=== {path} ===");
            println!("binders: {}", file.binders().len());
            for (i, binder) in file.binders().iter().enumerate() {
                println!("binder[{i}]:");
                dump_block(binder, 1);
            }
        }
    }

    println!("files_read={files_read} files_with_binders={files_with_binders}");
    for (bd_no, count) in &bdno_hist {
        println!("  BdNo={bd_no}: x{count}");
    }
}
