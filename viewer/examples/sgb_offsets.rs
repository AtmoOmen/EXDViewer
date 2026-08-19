//! The scene header's own offset table, so the slots nothing reads can be told apart from the slots
//! that are simply empty.

use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let bytes: Vec<u8> = match ironworks.file(&path) {
            Ok(held) => held,
            Err(why) => {
                println!("{path}: {why}");
                continue;
            }
        };
        // The section header sits after the container's own, and the body after two empty fields
        // where the older layout has them.
        let Some(at) = (0..bytes.len().saturating_sub(4)).find(|at| &bytes[*at..at + 4] == b"SCN1")
        else {
            println!("{path}: no SCN1");
            continue;
        };
        let word = |offset: usize| {
            i32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ])
        };
        let body = match (word(at + 8), word(at + 12)) {
            (0, 0) => at + 16,
            _ => at + 8,
        };
        let held: Vec<String> = (0..16)
            .map(|slot| {
                let value = word(body + slot * 4);
                match value {
                    0 => format!("{slot}:-"),
                    held => format!("{slot}:{held:#x}"),
                }
            })
            .collect();
        println!(
            "{:<62} SCN1 at {at:#x}  {}",
            path.rsplit('/').next().unwrap_or(&path),
            held.join(" ")
        );
    }
}
