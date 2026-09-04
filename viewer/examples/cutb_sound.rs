//! What a cutscene sounds, and how much of it the install holds: the `.scd` entry each `C063`
//! names, the voice file each `C048` key builds a path to, and the streaming track `C114` carries.
//!
//! `cutb_sound <paths file>` reads one `.cutb` path per line.

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::scd::SoundContainer;
use ironworks::file::tmb::{CommandKind, Item};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// The language codes the client writes into a voice path, in its own cutscene-language order.
const LANGUAGES: [&str; 4] = ["ja", "en", "de", "fr"];

/// `sub_14185AE20`: the key's second, third and fourth `_` parts, under the cutscene's expansion.
fn voice(key: &str, slug: &str, sex: char, language: &str) -> Option<String> {
    let mut parts = key.splitn(5, '_');
    let (_, quest, line, speaker) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if quest.len() < 6 || line.is_empty() || speaker.is_empty() {
        return None;
    }
    let folder = &quest[..6];
    Some(format!(
        "cut/{slug}/sound/{folder}/{quest}_{line}/vo_{quest}_{line}_{speaker}_{sex}_{language}.scd"
    ))
}

#[derive(Default)]
struct Tally {
    files: usize,
    effects: usize,
    /// Each `.scd` a `C063` names, with the entries it asks for.
    containers: BTreeMap<String, BTreeSet<i32>>,
    /// Each subtitle key, with the expansion its cutscene sits in.
    keys: BTreeSet<(String, String)>,
    tracks: usize,
    samples: usize,
    steps: BTreeSet<i32>,
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a path list");
    let paths = std::fs::read_to_string(list).expect("the list");

    let mut tally = Tally::default();
    for path in paths.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        tally.files += 1;
        let slug = path.split('/').nth(1).unwrap_or("ffxiv").to_owned();
        for node in file.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            for item in timeline.items() {
                let Item::Command(command) = item else {
                    continue;
                };
                match command.kind() {
                    CommandKind::C063(sound) => {
                        tally.effects += 1;
                        if let Some(held) = sound.path().filter(|held| !held.is_empty()) {
                            tally
                                .containers
                                .entry(held.to_owned())
                                .or_default()
                                .insert(sound.sound_index());
                        }
                    }
                    CommandKind::C048(subtitle) => {
                        if let Some(key) = subtitle.key().filter(|key| !key.is_empty()) {
                            tally.keys.insert((slug.clone(), key.to_ascii_lowercase()));
                        }
                    }
                    CommandKind::C114(track) => {
                        tally.tracks += 1;
                        tally.samples += track.samples().len();
                        tally.steps.insert(track.step());
                    }
                    _ => {}
                }
            }
        }
    }

    println!(
        "{} cutscenes: {} C063 over {} containers, {} subtitle keys, {} C114 holding {} samples \
         at steps {:?}",
        tally.files,
        tally.effects,
        tally.containers.len(),
        tally.keys.len(),
        tally.tracks,
        tally.samples,
        tally.steps
    );

    let (mut present, mut absent, mut inside, mut past) = (0usize, 0usize, 0usize, 0usize);
    for (path, entries) in &tally.containers {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            absent += 1;
            continue;
        };
        present += 1;
        let Ok(container) = SoundContainer::read(std::io::Cursor::new(bytes)) else {
            println!("{path} does not parse");
            continue;
        };
        for entry in entries {
            match (*entry as usize) < container.entries().len() {
                true => inside += 1,
                false => past += 1,
            }
        }
    }
    println!("containers: {present} present, {absent} absent; entries {inside} in range, {past} past the end");

    let mut spoken: BTreeMap<&str, usize> = BTreeMap::new();
    let mut sexes: BTreeMap<&str, usize> = BTreeMap::new();
    for (slug, key) in &tally.keys {
        let has = |sex: char, language: &str| {
            voice(key, slug, sex, language)
                .is_some_and(|path| ironworks.file::<Vec<u8>>(&path).is_ok())
        };
        for language in LANGUAGES {
            if has('m', language) || has('f', language) {
                *spoken.entry(language).or_default() += 1;
            }
        }
        let shape = match (has('m', "en"), has('f', "en")) {
            (true, true) => "both",
            (true, false) => "m only",
            (false, true) => "f only",
            (false, false) => continue,
        };
        *sexes.entry(shape).or_default() += 1;
    }
    println!("voice files by language: {spoken:?}");
    println!("voiced keys by sex: {sexes:?}");
}
