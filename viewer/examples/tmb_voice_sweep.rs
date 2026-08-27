//! Whether any standalone `.tmb` timeline carries a `C216` subtitle or a `C063` sound under
//! `sound/voice/`.
//!
//! `tmb_voice_sweep <paths file>` reads one `.tmb` path per line.

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

    let (mut files, mut subtitles, mut voice_hits, mut other_c063) =
        (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0;
    for path in paths.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(timeline) = Timeline::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        files += 1;
        for item in timeline.items() {
            let Item::Command(command) = item else {
                continue;
            };
            match command.kind() {
                CommandKind::C216(subtitle) => {
                    subtitles += 1;
                    if shown < 20 {
                        println!(
                            "{path}  time={} type={} text_id={} speaker_id={} duration={}",
                            command.time(),
                            subtitle.subtitle_type(),
                            subtitle.text_id(),
                            subtitle.speaker_id(),
                            subtitle.duration(),
                        );
                        shown += 1;
                    }
                }
                CommandKind::C063(sound) => match sound.path() {
                    Some(sound_path) if sound_path.to_lowercase().contains("voice") => {
                        voice_hits += 1;
                        if shown < 40 {
                            println!(
                                "{path}  time={} loop_duration={} -> {sound_path}",
                                command.time(),
                                sound.loop_duration(),
                            );
                            shown += 1;
                        }
                    }
                    _ => other_c063 += 1,
                },
                _ => {}
            }
        }
    }

    println!(
        "{files} tmb read, {subtitles} C216, {voice_hits} voice C063, {other_c063} other C063"
    );
}
