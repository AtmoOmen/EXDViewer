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
    /// How many of each unmodelled node, the bytes they spend, and one file holding one.
    unread: BTreeMap<String, (usize, usize, String)>,
    extensions: BTreeMap<String, BTreeMap<u32, usize>>,
    participants: BTreeMap<u32, usize>,
    /// Participants whose id is not `0xff000000 | (index + 1)`.
    misnumbered: usize,
    /// Participants holding a rotation outside a half turn either way, and the widest angle any
    /// of them reaches.
    unturned: usize,
    widest: f32,
    actors: usize,
    /// Actors whose participant the same file's `CTAL` holds.
    stood_for: usize,
    /// Camera bindings shaped like a participant id, and how many of those resolve.
    bound: [usize; 2],
    /// Cameras whose last modelled field is neither zero nor one, which a short body would leave.
    overrun: usize,
    groups: usize,
    runs: usize,
    values: usize,
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
                census.take(path, &file);
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
    fn take(&mut self, path: &str, file: &Cutscene) {
        let ids: BTreeSet<u32> = file
            .nodes()
            .iter()
            .filter_map(|node| match node {
                Node::Participants(participants) => Some(participants),
                _ => None,
            })
            .flatten()
            .map(|participant| participant.id())
            .collect();

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
                        let widest = participant
                            .rotation()
                            .iter()
                            .fold(0.0f32, |held, angle| held.max(angle.abs()));
                        self.unturned += usize::from(widest > std::f32::consts::PI);
                        self.widest = self.widest.max(widest);
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
                Node::Timeline(timeline) => self.timeline(timeline, &ids),
                Node::Unknown(unknown) => {
                    let entry = self
                        .unread
                        .entry(String::from_utf8_lossy(&unknown.magic()).into_owned())
                        .or_default();
                    entry.0 += 1;
                    entry.1 += unknown.body().len();
                    entry.2 = path.to_owned();
                }
                Node::Tracks(tracks) => {
                    self.runs += tracks.len();
                    self.values += tracks.iter().map(|track| track.values().len()).sum::<usize>();
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

    fn timeline(&mut self, timeline: &Timeline, ids: &BTreeSet<u32>) {
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
            match item {
                Item::Unknown(unknown) => {
                    entry.1.insert(unknown.body().len() + 8);
                }
                // A command spends four bytes on its id and its time ahead of the body.
                Item::Command(command) => {
                    if let CommandKind::Unknown { body, .. } = command.kind() {
                        entry.1.insert(body.len() + 12);
                    }
                }
                _ => {}
            }

            match item {
                Item::Actor(actor) => {
                    self.actors += 1;
                    self.stood_for += usize::from(ids.contains(&actor.participant()));
                }
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
                        self.overrun += usize::from(camera.bindings()[16] > 1);
                        for held in camera.bindings() {
                            if held >> 24 == 0xff && *held != u32::MAX {
                                self.bound[0] += 1;
                                self.bound[1] += usize::from(ids.contains(held));
                            }
                        }
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
        for (magic, (count, bytes, path)) in &self.unread {
            println!("  {magic}  {count:>7}  {bytes} unread bytes, last in {path}");
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
            "  {} holding a rotation past a half turn, the widest {}",
            self.unturned, self.widest
        );
        println!(
            "\n{} actors, {} standing for a participant the same file holds",
            self.actors, self.stood_for
        );
        println!("\n{} runs of scalars holding {} values", self.runs, self.values);
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
        println!(
            "  {} bindings shaped like a participant, {} of them resolving",
            self.bound[0], self.bound[1]
        );
        println!("  {} whose last modelled field is neither zero nor one", self.overrun);
    }
}

fn magic(node: &Node) -> &str {
    match node {
        Node::Resources(_) => "CTRL",
        Node::Sheet(_) => "CTIS",
        Node::Scene(_) => "CTDS",
        Node::Participants(_) => "CTAL",
        Node::Groups(_) => "CTPA",
        Node::Tracks(_) => "CTEX",
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
