//! Whether a cutscene's own `C009`/`C010` animation references lead to a `.pap` whose embedded
//! timeline carries a `C063` sound under `sound/voice/`.
//!
//! `cutb_pap_voice <paths file>` reads one `.cutb` path per line.

use std::collections::HashSet;

use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::pap::AnimationPack;
use ironworks::file::tmb::{CommandKind, Item, Timeline};
use ironworks::file::File;
use ironworks::{
    sqpack::{Install, SqPack},
    Ironworks,
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a path list");
    let paths = std::fs::read_to_string(list).expect("the list");

    let mut pap_paths: HashSet<String> = HashSet::new();
    let mut cutb_count = 0;
    for path in paths.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        cutb_count += 1;
        for node in file.nodes() {
            let Node::Resources(list) = node else {
                continue;
            };
            for resource in list.iter() {
                if resource.path().ends_with(".pap") {
                    pap_paths.insert(resource.path().to_owned());
                }
            }
        }
    }

    println!(
        "{cutb_count} cutb files named {} distinct pap paths",
        pap_paths.len()
    );
    for sample in pap_paths.iter().take(10) {
        println!("sample pap path: {sample}");
    }

    let (mut fetch_ok, mut read_ok, mut voice_hits, mut other_c063, mut subtitles) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut shown = 0;
    for pap_path in &pap_paths {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(pap_path) else {
            continue;
        };
        fetch_ok += 1;
        let Ok(pack) = AnimationPack::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        read_ok += 1;
        for timeline_bytes in pack.timelines() {
            if timeline_bytes.len() < 4 {
                continue;
            }
            let Ok(timeline) = Timeline::read(std::io::Cursor::new(timeline_bytes.clone())) else {
                continue;
            };
            for item in timeline.items() {
                let Item::Command(command) = item else {
                    continue;
                };
                match command.kind() {
                    CommandKind::C063(sound) => match sound.path() {
                        Some(sound_path) if sound_path.to_lowercase().contains("voice") => {
                            voice_hits += 1;
                            if shown < 20 {
                                println!(
                                    "{pap_path}  cmd.time={} loop_duration={} -> {sound_path}",
                                    command.time(),
                                    sound.loop_duration(),
                                );
                                shown += 1;
                            }
                        }
                        _ => other_c063 += 1,
                    },
                    CommandKind::C216(_) => subtitles += 1,
                    _ => {}
                }
            }
        }
    }

    println!(
        "{fetch_ok}/{} fetched, {read_ok} parsed, {voice_hits} voice C063, {other_c063} other C063, \
         {subtitles} C216 subtitles",
        pap_paths.len()
    );
}
