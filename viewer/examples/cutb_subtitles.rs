//! What a cutscene's `C048` subtitles state: the row each names, whether the sheet its `CTIS`
//! holds that row, and how long the line stands in each language.
//!
//! `cutb_subtitles <paths file>` reads one `.cutb` path per line.

use std::collections::BTreeMap;

use ironworks::excel::{Excel, Field, Language};
use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::tmb::{CommandKind, Item};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// Which of a `C048`'s captions the English client reads.
const ENGLISH: usize = 1;

fn key_of(field: &Field) -> Option<String> {
    match field {
        Field::String(held) => Some(String::from_utf8_lossy(held.as_bytes()).into_owned()),
        _ => None,
    }
}

#[derive(Default)]
struct Tally {
    files: usize,
    named: usize,
    subtitles: usize,
    resolved: usize,
    keyless: usize,
    captions: BTreeMap<usize, usize>,
    kinds: BTreeMap<i32, usize>,
    enabled: [usize; 8],
    voices: usize,
    /// Keys whose own id is the sheet's last segment, against those that read some other way, and
    /// how many name a speaker past the line number.
    id_matches: usize,
    id_differs: usize,
    trailing_digits: usize,
}

fn main() {
    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);
    let list = std::env::args().nth(1).expect("a path list");
    let paths = std::fs::read_to_string(list).expect("the list");

    let mut tally = Tally::default();
    let mut shown = 0;
    for path in paths.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        tally.files += 1;

        let sheet = file.nodes().iter().find_map(|node| match node {
            Node::Sheet(name) => Some(name.clone()),
            _ => None,
        });
        let rows: BTreeMap<String, String> = sheet
            .as_deref()
            .and_then(|name| excel.sheet(name).ok())
            .map(|sheet| {
                let columns = sheet.columns().unwrap_or_default();
                let mut held = BTreeMap::new();
                if let [key, text, ..] = columns.as_slice() {
                    for row in sheet {
                        let (Some(key), Some(text)) = (
                            row.field(key).ok().as_ref().and_then(key_of),
                            row.field(text).ok().as_ref().and_then(key_of),
                        ) else {
                            continue;
                        };
                        held.insert(key, text);
                    }
                }
                held
            })
            .unwrap_or_default();
        tally.named += usize::from(sheet.is_some());
        let id_upper = sheet
            .as_deref()
            .and_then(|name| name.rsplit('/').next())
            .unwrap_or_default()
            .to_uppercase();

        for node in file.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            for item in timeline.items() {
                let Item::Command(command) = item else {
                    continue;
                };
                match command.kind() {
                    CommandKind::C048(subtitle) => {
                        tally.subtitles += 1;
                        *tally.captions.entry(subtitle.captions().len()).or_default() += 1;
                        *tally.kinds.entry(subtitle.subtitle_type()).or_default() += 1;
                        for (slot, caption) in subtitle.captions().iter().enumerate().take(8) {
                            tally.enabled[slot] += usize::from(caption.enabled() != 0);
                        }
                        let Some(key) = subtitle.key() else {
                            tally.keyless += 1;
                            continue;
                        };
                        match key
                            .strip_prefix("TEXT_")
                            .is_some_and(|rest| rest.starts_with(&id_upper))
                        {
                            true => tally.id_matches += 1,
                            false => tally.id_differs += 1,
                        }
                        tally.trailing_digits += usize::from(
                            key.rsplit('_')
                                .next()
                                .is_some_and(|last| last.bytes().all(|byte| byte.is_ascii_digit())),
                        );
                        match rows.get(key) {
                            Some(text) => {
                                tally.resolved += 1;
                                if shown < 12 {
                                    shown += 1;
                                    println!(
                                        "{path} t={} {}ms {key}\n  {text}",
                                        command.time(),
                                        subtitle
                                            .captions()
                                            .get(ENGLISH)
                                            .map(|caption| caption.duration())
                                            .unwrap_or(0),
                                    );
                                }
                            }
                            None => println!("{path}: {} names no row", key),
                        }
                    }
                    CommandKind::C063(sound) => {
                        tally.voices += usize::from(
                            sound
                                .path()
                                .is_some_and(|path| path.to_lowercase().contains("voice")),
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    println!(
        "{} files, {} naming a sheet, {} C048 subtitles, {} resolving a row, {} naming none",
        tally.files, tally.named, tally.subtitles, tally.resolved, tally.keyless,
    );
    println!("captions a subtitle holds: {:?}", tally.captions);
    println!("subtitle_type: {:?}", tally.kinds);
    println!("captions enabled by slot: {:?}", tally.enabled);
    println!("{} voice C063", tally.voices);
    println!(
        "{} keys open with the sheet's own id, {} read some other way, {} end in a number rather \
         than a speaker",
        tally.id_matches, tally.id_differs, tally.trailing_digits,
    );
}
