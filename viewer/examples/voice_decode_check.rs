//! Decodes a sample of real `sound/voice/` files through the same path `.scd` playback uses, to
//! confirm the codecs shipped there decode.
//!
//! `voice_decode_check <paths file> [count]`

use ironworks::file::scd::SoundContainer;
use ironworks::file::File;
use ironworks::sqpack::{Install, SqPack};
use ironworks::Ironworks;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a path list");
    let count: usize = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(50);
    let paths = std::fs::read_to_string(list).expect("the list");

    let (mut ok, mut failed) = (0usize, 0usize);
    for path in paths.lines().take(count) {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            println!("fetch failed: {path}");
            failed += 1;
            continue;
        };
        let Ok(container) = SoundContainer::read(std::io::Cursor::new(bytes)) else {
            println!("scd parse failed: {path}");
            failed += 1;
            continue;
        };
        let Some(entry) = container.entries().first() else {
            println!("no entries: {path}");
            failed += 1;
            continue;
        };
        match viewer::audio::decode(entry) {
            Ok(decoded) => {
                ok += 1;
                if ok <= 5 {
                    println!(
                        "{path}: {} ch, {} Hz, {} samples",
                        decoded.channels,
                        decoded.sample_rate,
                        decoded.samples.len()
                    );
                }
            }
            Err(error) => {
                failed += 1;
                println!("decode failed: {path}: {error}");
            }
        }
    }

    println!("{ok} decoded, {failed} failed, out of {count}");
}
