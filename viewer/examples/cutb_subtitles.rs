//! Where a cutscene's `C216` subtitles and `C063` sounds sit in time, relative to the actor and
//! track that carry them.
//!
//! `cutb_subtitles <paths file>` reads one `.cutb` path per line.

use std::collections::HashMap;

use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::tmb::{CommandKind, Item};
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

    let (mut files, mut subtitles, mut voices, mut other_sounds) = (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0;
    for path in paths.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        files += 1;

        for node in file.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            let items = timeline.items();

            // Which track (if any) nests a given command id, and that track's own time.
            let mut track_time: HashMap<i16, i16> = HashMap::new();
            for item in items {
                if let Item::Track(track) = item {
                    for command_id in track.commands() {
                        track_time.insert(*command_id, track.time());
                    }
                }
            }

            for item in items {
                let Item::Command(command) = item else {
                    continue;
                };
                match command.kind() {
                    CommandKind::C216(subtitle) => {
                        subtitles += 1;
                        if shown < 30 {
                            println!(
                                "{path}  cmd.time={} track.time={:?} enabled={} type={} text_id={} \
                                 speaker_id={} duration={}",
                                command.time(),
                                track_time.get(&command.id()),
                                subtitle.enabled(),
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
                            voices += 1;
                            println!(
                                "{path}  cmd.time={} track.time={:?} loop_duration={} sound_index={} \
                                 -> {sound_path}",
                                command.time(),
                                track_time.get(&command.id()),
                                sound.loop_duration(),
                                sound.sound_index(),
                            );
                        }
                        _ => other_sounds += 1,
                    },
                    _ => {}
                }
            }
        }
    }

    println!(
        "{files} files, {subtitles} C216 subtitles, {voices} voice C063, {other_sounds} other C063"
    );
}
