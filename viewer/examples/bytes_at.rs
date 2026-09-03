//! The bytes a file holds at an offset, read every way a header field is usually written.
//!
//! `bytes_at <offset> <path> [more paths]`

use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let at: usize = args
        .next()
        .and_then(|held| match held.strip_prefix("0x") {
            Some(hex) => usize::from_str_radix(hex, 16).ok(),
            None => held.parse().ok(),
        })
        .expect("an offset");
    for path in args {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(&path) else {
            println!("{path}: could not be read");
            continue;
        };
        let Some(held) = bytes.get(at..at + 16) else {
            println!("{path}: only {} bytes", bytes.len());
            continue;
        };
        let four = |from: usize| -> [u8; 4] { held[from..from + 4].try_into().unwrap() };
        println!("{}", path.rsplit('/').next().unwrap_or(&path));
        print!("   +{at:#06x} ");
        for byte in held {
            print!("{byte:02x} ");
        }
        println!();
        println!(
            "      f32 {:>12.4} {:>12.4} {:>12.4} {:>12.4}",
            f32::from_le_bytes(four(0)),
            f32::from_le_bytes(four(4)),
            f32::from_le_bytes(four(8)),
            f32::from_le_bytes(four(12)),
        );
        println!(
            "      u32 {:>12} {:>12} {:>12} {:>12}",
            u32::from_le_bytes(four(0)),
            u32::from_le_bytes(four(4)),
            u32::from_le_bytes(four(8)),
            u32::from_le_bytes(four(12)),
        );
    }
}
