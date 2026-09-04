//! What a cutscene's `C048` subtitles state: the row each names, whether the sheet its `CTIS`
//! holds that row, how long the line stands in each language, and the frame rate the gaps between
//! them bound from above.
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
    /// The tightest frames-a-second any two subtitles of one timeline leave room for: a line
    /// cannot outlast the gap to the next.
    ceiling: f32,
    ceiling_at: String,
}

fn main() {
    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);
    let list = std::env::args().nth(1).expect("a path list");
    let paths = std::fs::read_to_string(list).expect("the list");

    let mut tally = Tally {
        ceiling: f32::INFINITY,
        ..Tally::default()
    };
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

        for node in file.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            let mut spans: Vec<(f32, f32)> = Vec::new();
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
                        if let Some(caption) = subtitle.captions().get(ENGLISH)
                            && caption.enabled() != 0
                        {
                            spans.push((f32::from(command.time()), caption.duration() as f32));
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
            spans.sort_by(|left, right| left.0.total_cmp(&right.0));
            for pair in spans.windows(2) {
                let (frames, duration) = (pair[1].0 - pair[0].0, pair[0].1);
                if frames > 0.0 && duration > 0.0 {
                    let ceiling = frames * 1000.0 / duration;
                    if ceiling < tally.ceiling {
                        tally.ceiling = ceiling;
                        tally.ceiling_at = format!("{path} at frame {}", pair[0].0);
                    }
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
        "a subtitle never outlasts the gap to the next past {:.2} frames a second ({})",
        tally.ceiling, tally.ceiling_at,
    );
}
