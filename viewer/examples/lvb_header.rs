//! Every slot of an `.lvb` scene header, so the unmodelled ones can be seen side by side.
//!
//! `lvb_header <zone> [more zones]` where a zone is what the level viewer opens, e.g.
//! `ex1/01_roc_r2/twn/r2t1`.

use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// Slots of the header's own offset table.
const FIELDS: usize = 16;

/// Slots of the general block to show.
const GENERAL: usize = 24;

fn i32_at(bytes: &[u8], at: usize) -> i32 {
    bytes
        .get(at..at + 4)
        .map_or(0, |held| i32::from_le_bytes(held.try_into().unwrap()))
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let zones: Vec<String> = std::env::args().skip(1).collect();
    let mut rows: Vec<(String, Vec<i32>)> = Vec::new();
    for zone in &zones {
        let stem = zone.rsplit('/').next().unwrap_or(zone);
        let path = match zone.starts_with("ffxiv/") || zone.starts_with("ex") {
            true => format!("bg/{zone}/level/{stem}.lvb"),
            false => format!("bg/ffxiv/{zone}/level/{stem}.lvb"),
        };
        let Ok(bytes) = ironworks.file::<Vec<u8>>(&path) else {
            println!("{zone}: could not be read");
            continue;
        };
        // The scene section follows the file header, and an older one puts two empty fields ahead
        // of its body.
        let at = bytes
            .windows(4)
            .position(|four| four == b"SCN1")
            .unwrap_or(12);
        let body = match (i32_at(&bytes, at + 8), i32_at(&bytes, at + 12)) {
            (0, 0) => at + 16,
            _ => at + 8,
        };
        let offsets: Vec<i32> = (0..FIELDS)
            .map(|slot| i32_at(&bytes, body + slot * 4))
            .collect();
        let general = body + offsets[2].max(0) as usize;
        println!(
            "{stem:<8} SCN1 at {at:#06x}  body {body:#06x}  general {general:#06x}  offsets {:?}",
            offsets
        );
        rows.push((
            stem.to_owned(),
            (0..GENERAL)
                .map(|slot| i32_at(&bytes, general + slot * 4))
                .collect(),
        ));
    }
    println!();
    print!("{:<10}", "general +");
    for (name, _) in &rows {
        print!("{name:>12}");
    }
    println!();
    for slot in 0..GENERAL {
        print!("{:<10}", format!("{:#06x}", slot * 4));
        for (_, held) in &rows {
            print!("{:>12}", held[slot]);
        }
        println!();
    }
}
