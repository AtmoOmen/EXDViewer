//! What the cutscenes hold: the nodes, the participants, the timeline commands, and the cameras
//! among them.
//!
//! `cutb_census <paths file>`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::tmb::{CommandKind, Item, Timeline};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

#[derive(Default)]
struct Census {
    read: usize,
    failed: usize,
    /// How many of each node magic, and how many a file ever holds at once.
    nodes: BTreeMap<String, (usize, BTreeSet<usize>)>,
    /// Bytes each unmodelled node spends, by magic.
    unread: BTreeMap<String, usize>,
    extensions: BTreeMap<String, BTreeMap<u32, usize>>,
    participants: BTreeMap<u32, usize>,
    /// Participants whose id is not `0xff000000 | (index + 1)`.
    misnumbered: usize,
    groups: usize,
    /// `CTPA` records, split by whether the first field names a participant.
    records: [usize; 2],
    /// How many of each timeline item magic, and the body sizes the unmodelled ones take.
    items: BTreeMap<String, (usize, BTreeSet<usize>)>,
    /// Which channel each curve drives, by tag.
    tags: BTreeMap<u8, usize>,
    cameras: usize,
    /// Cameras whose curve id names a `TMFC` in the same timeline.
    curved: usize,
    planes: BTreeMap<(u32, u32), usize>,
    names: BTreeSet<String>,
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a paths file");
    let paths = std::fs::read_to_string(list).expect("the paths file");

    let mut census = Census::default();
    for path in paths.lines().filter(|path| path.ends_with(".cutb")) {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        match Cutscene::read(std::io::Cursor::new(bytes)) {
            Ok(file) => {
                census.read += 1;
                census.take(&file);
            }
            Err(error) => {
                census.failed += 1;
                println!("{path}: {error}");
            }
        }
    }
    census.report();
}

impl Census {
    fn take(&mut self, file: &Cutscene) {
        let mut held: BTreeMap<String, usize> = BTreeMap::new();
        for node in file.nodes() {
            *held.entry(magic(node).to_owned()).or_default() += 1;
            match node {
                Node::Resources(resources) => {
                    for resource in resources {
                        let extension = resource.path().rsplit('.').next().unwrap_or_default();
                        *self
                            .extensions
                            .entry(extension.to_owned())
                            .or_default()
                            .entry(resource.unknown_1())
                            .or_default() += 1;
                    }
                }
                Node::Participants(participants) => {
                    for (index, participant) in participants.iter().enumerate() {
                        *self.participants.entry(participant.kind()).or_default() += 1;
                        let expected = 0xff00_0000 | (index as u32 + 1);
                        self.misnumbered += usize::from(participant.id() != expected);
                    }
                }
                Node::Groups(groups) => {
                    self.groups += groups.len();
                    for group in groups {
                        for record in group.records() {
                            let head = u32::from_le_bytes(record[..4].try_into().unwrap());
                            self.records[usize::from(head >> 24 == 0xff)] += 1;
                        }
                    }
                }
                Node::Timeline(timeline) => self.timeline(timeline),
                Node::Unknown(unknown) => {
                    *self
                        .unread
                        .entry(String::from_utf8_lossy(&unknown.magic()).into_owned())
                        .or_default() += unknown.body().len();
                }
                Node::Sheet(_) | Node::Scene(_) => {}
            }
        }
        for (magic, count) in held {
            let entry = self.nodes.entry(magic).or_default();
            entry.0 += count;
            entry.1.insert(count);
        }
    }

    fn timeline(&mut self, timeline: &Timeline) {
        let sets: BTreeSet<i16> = timeline
            .items()
            .iter()
            .filter_map(|item| match item {
                Item::Curves(curves) => Some(curves.id()),
                _ => None,
            })
            .collect();

        for item in timeline.items() {
            let entry = self.items.entry(magic_of(item)).or_default();
            entry.0 += 1;
            if let Item::Unknown(unknown) = item {
                entry.1.insert(unknown.body().len() + 8);
            }

            match item {
                Item::Curves(curves) => {
                    for curve in curves.curves() {
                        *self.tags.entry(curve.tag()).or_default() += 1;
                    }
                }
                Item::Command(command) => {
                    if let CommandKind::C004(camera) = command.kind() {
                        self.cameras += 1;
                        self.curved += usize::from(
                            i16::try_from(camera.curve_id()).is_ok_and(|id| sets.contains(&id)),
                        );
                        *self
                            .planes
                            .entry((camera.near_plane().to_bits(), camera.far_plane().to_bits()))
                            .or_default() += 1;
                        self.names.insert(camera.name().unwrap_or_default().to_owned());
                    }
                }
                _ => {}
            }
        }
    }

    fn report(&self) {
        println!("\n{} read, {} failed", self.read, self.failed);

        println!("\nnodes");
        for (magic, (count, held)) in &self.nodes {
            let held: Vec<String> = held.iter().map(usize::to_string).collect();
            println!("  {magic}  {count:>7}  per file {}", held.join(","));
        }
        for (magic, bytes) in &self.unread {
            println!("  {magic} holds {bytes} unread bytes");
        }

        println!("\nresources by extension, and the flag beside each");
        for (extension, flags) in &self.extensions {
            let flags: Vec<String> = flags
                .iter()
                .map(|(flag, count)| format!("{flag}: {count}"))
                .collect();
            println!("  {extension:>5}  {}", flags.join(", "));
        }

        println!("\nparticipants by kind: {:?}", self.participants);
        println!("  {} numbered outside 0xff000000 | (index + 1)", self.misnumbered);
        println!(
            "\n{} groups holding {} records naming a participant and {} naming something else",
            self.groups, self.records[1], self.records[0]
        );

        println!("\ntimeline items, and the sizes the unmodelled ones take");
        for (magic, (count, sizes)) in &self.items {
            let sizes: Vec<String> = sizes.iter().map(usize::to_string).collect();
            println!("  {magic}  {count:>7}  {}", sizes.join(","));
        }

        println!("\ncurve tags, as the block and the component of it");
        for (tag, count) in &self.tags {
            println!("  {tag:#04x}  block {:>2} component {:>2}  {count}", tag >> 4, tag & 0xF);
        }

        println!("\n{} cameras, {} naming a curve set beside them", self.cameras, self.curved);
        for ((near, far), count) in &self.planes {
            println!(
                "  near {} far {}: {count}",
                f32::from_bits(*near),
                f32::from_bits(*far)
            );
        }
        println!("  {} distinct shot names", self.names.len());
    }
}

fn magic(node: &Node) -> &str {
    match node {
        Node::Resources(_) => "CTRL",
        Node::Sheet(_) => "CTIS",
        Node::Scene(_) => "CTDS",
        Node::Participants(_) => "CTAL",
        Node::Groups(_) => "CTPA",
        Node::Timeline(_) => "CTTL",
        Node::Unknown(_) => "?",
    }
}

fn magic_of(item: &Item) -> String {
    match item {
        Item::Header(_) => "TMDH".to_owned(),
        Item::FaceLibrary(_) => "TMPP".to_owned(),
        Item::ActorList(_) => "TMAL".to_owned(),
        Item::Actor(_) => "TMAC".to_owned(),
        Item::Track(_) => "TMTR".to_owned(),
        Item::Curves(_) => "TMFC".to_owned(),
        Item::Unknown(unknown) => String::from_utf8_lossy(&unknown.magic()).into_owned(),
        // A modelled command debug-prints as its own magic ahead of the body.
        Item::Command(command) => match command.kind() {
            CommandKind::Unknown { magic, .. } => String::from_utf8_lossy(magic).into_owned(),
            held => format!("{held:?}")
                .split_once('(')
                .map(|(magic, _)| magic.to_owned())
                .unwrap_or_default(),
        },
    }
}
