//! Scratch tool: how many avfx files have every scheduled run and every particle life finite
//! (so a placement should loop them) versus at least one open end (so a placement should not).

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

fn life_finite(blocks: &[Block]) -> Option<bool> {
    let value = find(blocks, "Life")?.find("Val")?.f32()?;
    Some(value >= 0.0)
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

    let (mut bounded, mut unbounded, mut errors) = (0u64, 0u64, 0u64);
    let mut why_unbounded_run = 0u64;
    let mut why_unbounded_life = 0u64;

    for (i, path) in paths.iter().enumerate() {
        if i % 10000 == 0 {
            eprintln!("{i}/{}", paths.len());
        }
        let bytes: Vec<u8> = match ironworks.file(path) {
            Ok(b) => b,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        let file = match Avfx::read(Cursor::new(bytes)) {
            Ok(f) => f,
            Err(_) => {
                errors += 1;
                continue;
            }
        };

        // Reconstruct which timeline items actually run: any scheduler item, or every timeline
        // if no scheduler starts anything (mirrors sim::runs()).
        let mut any_run_unbounded = false;
        let mut saw_any_run = false;
        let mut visit_timeline = |index: usize| {
            let Some(timeline) = file.timelines().get(index) else {
                return;
            };
            for item in timeline.items() {
                let blocks = item.blocks();
                if find(blocks, "bEna").and_then(Block::bool) == Some(false) {
                    continue;
                }
                saw_any_run = true;
                let end = find(blocks, "EdTm").and_then(Block::i32).unwrap_or(-1);
                if end < 0 {
                    any_run_unbounded = true;
                }
            }
        };

        let mut scheduled_any = false;
        for scheduler in file.schedulers() {
            for item in scheduler.items() {
                let blocks = item.blocks();
                if find(blocks, "bEna").and_then(Block::bool) == Some(false) {
                    continue;
                }
                let Some(index) = find(blocks, "TlNo")
                    .and_then(Block::i32)
                    .and_then(|v| usize::try_from(v).ok())
                else {
                    continue;
                };
                scheduled_any = true;
                visit_timeline(index);
            }
        }
        if !scheduled_any {
            for index in 0..file.timelines().len() {
                visit_timeline(index);
            }
        }
        if !saw_any_run {
            for _ in 0..file.emitters().len() {
                any_run_unbounded = true;
            }
        }

        let any_life_unbounded = file
            .particles()
            .iter()
            .any(|particle| life_finite(particle.blocks()) != Some(true));

        if any_run_unbounded {
            why_unbounded_run += 1;
        }
        if any_life_unbounded {
            why_unbounded_life += 1;
        }

        match any_run_unbounded || any_life_unbounded {
            true => unbounded += 1,
            false => bounded += 1,
        }
    }

    let total = bounded + unbounded;
    println!("bounded (loops)      x{bounded:7}  ({:.1}%)", 100.0 * bounded as f64 / total as f64);
    println!("unbounded (no loop)  x{unbounded:7}  ({:.1}%)", 100.0 * unbounded as f64 / total as f64);
    println!("  of which: an open run   x{why_unbounded_run:7}");
    println!("  of which: an open life  x{why_unbounded_life:7}");
    println!("errors x{errors}");
}
