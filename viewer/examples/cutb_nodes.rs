//! What the nodes this crate does not model hold, and the shapes the modelled ones leave unnamed.
//!
//! `cutb_nodes <cutb path to dump, or a paths file to count>`

use std::collections::BTreeMap;

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// How many leading bytes a dump shows of a node.
const HEAD: usize = 128;

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn real(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn dump(magic: &str, body: &[u8]) {
    println!("\n{magic}: {} bytes", body.len());
    for at in (0..body.len().min(HEAD)).step_by(16) {
        let row = &body[at..(at + 16).min(body.len())];
        let hex: Vec<String> = row.iter().map(|byte| format!("{byte:02x}")).collect();
        let text: String = row
            .iter()
            .map(|byte| match byte.is_ascii_graphic() {
                true => char::from(*byte),
                false => '.',
            })
            .collect();
        let words: Vec<String> = (0..row.len() / 4)
            .map(|held| {
                let value = word(row, held * 4);
                match value < 0x1000 {
                    true => format!("{value:>10}"),
                    false => format!("{:>10.3}", real(row, held * 4)),
                }
            })
            .collect();
        println!("  {at:#06x}  {:<47}  {text:<16}  {}", hex.join(" "), words.join(" "));
    }
}

/// The strings a node ends with, which are what a text-bearing node is really carrying.
fn strings(body: &[u8]) -> Vec<String> {
    let mut held = Vec::new();
    let mut run = Vec::new();
    for byte in body {
        match byte.is_ascii_graphic() || *byte == b' ' {
            true => run.push(*byte),
            false => {
                if run.len() >= 3 {
                    held.push(String::from_utf8_lossy(&run).into_owned());
                }
                run.clear();
            }
        }
    }
    if run.len() >= 3 {
        held.push(String::from_utf8_lossy(&run).into_owned());
    }
    held
}

#[derive(Default)]
struct Shape {
    nodes: usize,
    bytes: usize,
    widest: usize,
    /// Bodies by their size to the nearest power of two.
    sizes: BTreeMap<u32, usize>,
    /// How many hold a run of readable text, and the runs themselves.
    texts: usize,
    words: BTreeMap<String, usize>,
    /// The largest body seen, and the file it came from.
    sample: Option<(String, Vec<u8>)>,
}

impl Shape {
    fn take(&mut self, path: &str, body: &[u8]) {
        self.nodes += 1;
        self.bytes += body.len();
        self.widest = self.widest.max(body.len());
        *self
            .sizes
            .entry(body.len().next_power_of_two().trailing_zeros())
            .or_default() += 1;

        let held = strings(body);
        self.texts += usize::from(!held.is_empty());
        for word in held {
            *self.words.entry(word).or_default() += 1;
        }

        if self.sample.as_ref().is_none_or(|(_, held)| held.len() < body.len()) {
            self.sample = Some((path.to_owned(), body.to_vec()));
        }
    }

    fn report(&self, magic: &str) {
        println!(
            "\n{magic}: {} nodes over {} bytes, the widest {}",
            self.nodes, self.bytes, self.widest
        );
        let sizes: Vec<String> = self
            .sizes
            .iter()
            .map(|(power, count)| format!("<={}: {count}", 1u64 << power))
            .collect();
        println!("  by size: {}", sizes.join(", "));
        println!(
            "  {} hold readable text, over {} distinct runs",
            self.texts,
            self.words.len()
        );
        let mut words: Vec<(&String, &usize)> = self.words.iter().collect();
        words.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let words: Vec<String> = words
            .iter()
            .take(12)
            .map(|(word, count)| format!("{word:?} x{count}"))
            .collect();
        println!("  commonest runs: {}", words.join(", "));
        if let Some((path, body)) = &self.sample {
            println!("  the widest, from {path}");
            dump(magic, body);
        }
    }
}

#[derive(Default)]
struct Census {
    read: usize,
    unread: BTreeMap<String, Shape>,
    /// `CTPA`'s middle dword, over twelve.
    groups: BTreeMap<u32, usize>,
    records: usize,
    /// How far a `CTAL` record's body reaches past its transform.
    participants: BTreeMap<usize, usize>,
    /// The authoring id at a `C004`'s `+0x10`, and how many are distinct within one file.
    cameras: usize,
    distinct: usize,
}

impl Census {
    fn report(&self) {
        println!("\n{} files", self.read);
        for (magic, shape) in &self.unread {
            shape.report(magic);
        }

        println!(
            "\nCTPA: {} records, their middle dword over twelve taking {} values",
            self.records,
            self.groups.len()
        );
        let mut held: Vec<(&u32, &usize)> = self.groups.iter().collect();
        held.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let held: Vec<String> = held
            .iter()
            .take(8)
            .map(|(value, count)| format!("{value}: {count}"))
            .collect();
        println!("  commonest: {}", held.join(", "));

        println!("\nCTAL record bodies, by the bytes they spend past the transform");
        let mut sizes: Vec<(&usize, &usize)> = self.participants.iter().collect();
        sizes.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (size, count) in sizes.iter().take(10) {
            println!("  {:>4} bytes of record: {count}", **size + 0x30);
        }
        println!("  {} distinct sizes", self.participants.len());

        println!(
            "\nC004 +0x10: {} cameras, {} of them holding an id no other camera in the same file \
             holds",
            self.cameras, self.distinct
        );
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let held = std::env::args().nth(1).expect("a cutb path or a paths file");

    if held.ends_with(".cutb") {
        let bytes = ironworks.file::<Vec<u8>>(&held).expect("the cutscene");
        let file = Cutscene::read(std::io::Cursor::new(bytes)).expect("a cutscene");
        for node in file.nodes() {
            if let Node::Unknown(unknown) = node {
                dump(&String::from_utf8_lossy(&unknown.magic()), unknown.body());
            }
        }
        return;
    }

    let paths = std::fs::read_to_string(held).expect("the paths file");
    let mut census = Census::default();
    for path in paths.lines().filter(|path| path.ends_with(".cutb")) {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        census.read += 1;

        let mut ids: Vec<u32> = Vec::new();
        for node in file.nodes() {
            match node {
                Node::Unknown(unknown) => census
                    .unread
                    .entry(String::from_utf8_lossy(&unknown.magic()).into_owned())
                    .or_default()
                    .take(path, unknown.body()),
                Node::Groups(groups) => {
                    for group in groups {
                        for record in group.records() {
                            census.records += 1;
                            *census.groups.entry(word(record, 4) / 12).or_default() += 1;
                        }
                    }
                }
                Node::Participants(participants) => {
                    for participant in participants {
                        *census
                            .participants
                            .entry(participant.body().len())
                            .or_default() += 1;
                    }
                }
                Node::Timeline(timeline) => {
                    for item in timeline.items() {
                        if let ironworks::file::tmb::Item::Command(command) = item {
                            if let ironworks::file::tmb::CommandKind::C004(camera) = command.kind()
                            {
                                ids.push(camera.unknown_1() as u32);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        census.cameras += ids.len();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        census.distinct += ids
            .iter()
            .filter(|id| sorted.iter().filter(|held| *held == *id).count() == 1)
            .count();
    }
    census.report();
}
