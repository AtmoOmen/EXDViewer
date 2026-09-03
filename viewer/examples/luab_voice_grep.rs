//! Whether any shipped `.luab` chunk references a `Voice`-named call at all, and how many name a
//! literal `sound/voice/` path in their string constants.
//!
//! `luab_voice_grep <paths file>` reads one `.luab` path per line.

use ironworks::sqpack::{Install, SqPack};
use ironworks::Ironworks;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a path list");
    let paths = std::fs::read_to_string(list).expect("the list");

    let (mut files, mut voice_word, mut voice_path) = (0usize, 0usize, 0usize);
    let mut shown = 0;
    for path in paths.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        files += 1;
        if bytes
            .windows(5)
            .any(|window| window.eq_ignore_ascii_case(b"Voice"))
        {
            voice_word += 1;
            if shown < 20 {
                println!("{path} mentions Voice");
                shown += 1;
            }
        }
        if bytes
            .windows(12)
            .any(|window| window.eq_ignore_ascii_case(b"sound/voice/"))
        {
            voice_path += 1;
            println!("{path} names a sound/voice/ literal");
        }
    }

    println!("{files} luab read, {voice_word} mention Voice, {voice_path} name sound/voice/");
}
