//! What the fade, the visibility and the unmodelled `CTCB` node hold across every cutscene.
//!
//! `cutb_fade <paths file>`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::layer::{Instance, InstanceData};
use ironworks::file::tmb::{CommandKind, Item, Timeline};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

#[derive(Default)]
struct Census {
    read: usize,
    fades: usize,
    /// Fades that end at nought, at one, and between.
    out: usize,
    into: usize,
    part: usize,
    /// Fades whose stated length is nought, which the client divides by.
    instant: usize,
    lengths: BTreeMap<i32, usize>,
    starts: BTreeMap<String, usize>,
    ends: BTreeMap<String, usize>,
    /// How many fades state a filter at all, and what those filters hold.
    filtered: usize,
    filters: BTreeMap<i32, usize>,
    enables: BTreeMap<i32, usize>,
    /// Fades whose participant no actor track reaches.
    unaddressed: usize,
    /// Participants carrying more than one fade, and the most any one carries. Overlap is two
    /// fades whose spans cross.
    overlapping: usize,
    stacked: usize,
    /// What each command runs against, by the kind of participant it addresses.
    fade_kinds: BTreeMap<String, usize>,
    shown_kinds: BTreeMap<String, usize>,
    shows: usize,
    unaddressed_shows: usize,
    effects: usize,
    unaddressed_effects: usize,
    /// `CTCB`: how many records, against how many timelines the file holds.
    ctcb: usize,
    ctcb_matches: usize,
    ctcb_fields: [BTreeMap<i32, usize>; 6],
    /// Whether one of the six fields is a permutation of `0..n`.
    permutations: [usize; 6],
    ctcb_over: BTreeMap<i64, usize>,
    link_permutations: usize,
    link_of_timelines: usize,
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
        if let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) {
            census.read += 1;
            census.take(&file);
        }
    }
    census.report();
}

/// The participant each command runs against.
fn addressed(timeline: &Timeline) -> BTreeMap<i16, u32> {
    let tracks: BTreeMap<i16, &[i16]> = timeline
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Track(track) => Some((track.id(), track.commands())),
            _ => None,
        })
        .collect();
    let mut held = BTreeMap::new();
    for item in timeline.items() {
        let Item::Actor(actor) = item else { continue };
        for track in actor.tracks() {
            for command in tracks.get(track).into_iter().flat_map(|held| held.iter()) {
                held.insert(*command, actor.participant());
            }
        }
    }
    held
}

fn kind_of(participants: &[Instance], id: u32) -> String {
    match participants.iter().find(|held| held.id() == id) {
        Some(held) => match held.data() {
            InstanceData::HelperObject(helper) => format!("{:?}", helper.kind()),
            data => format!("{data:?}").split('(').next().unwrap_or("?").to_owned(),
        },
        None => "absent".to_owned(),
    }
}

fn bucket(value: f32) -> String {
    match value {
        v if v <= 0.0 => "0".to_owned(),
        v if v >= 1.0 => "1".to_owned(),
        v => format!("{:.1}", (v * 10.0).round() / 10.0),
    }
}

impl Census {
    fn take(&mut self, file: &Cutscene) {
        let participants = file
            .nodes()
            .iter()
            .find_map(|node| match node {
                Node::Participants(held) => Some(held.as_slice()),
                _ => None,
            })
            .unwrap_or_default();
        let timelines: Vec<&Timeline> = file
            .nodes()
            .iter()
            .filter_map(|node| match node {
                Node::Timeline(held) => Some(held),
                _ => None,
            })
            .collect();

        // Every fade a participant carries, as (start frame, length), for the overlap count.
        let mut spans: BTreeMap<u32, Vec<(f32, f32)>> = BTreeMap::new();
        for timeline in &timelines {
            let addressed = addressed(timeline);
            for item in timeline.items() {
                let Item::Command(command) = item else {
                    continue;
                };
                let target = addressed.get(&command.id()).copied();
                match command.kind() {
                    CommandKind::C094(fade) => {
                        self.fades += 1;
                        let (from, to) = (fade.start_visibility(), fade.end_visibility());
                        *self.starts.entry(bucket(from)).or_default() += 1;
                        *self.ends.entry(bucket(to)).or_default() += 1;
                        match to {
                            v if v <= 0.0 => self.out += 1,
                            v if v >= 1.0 => self.into += 1,
                            _ => self.part += 1,
                        }
                        let length = fade.fade_time();
                        self.instant += usize::from(length == 0);
                        *self.lengths.entry(length.min(300)).or_default() += 1;
                        match fade.filter() {
                            Some(filter) => {
                                self.filtered += 1;
                                *self.filters.entry(filter.filter()).or_default() += 1;
                                *self.enables.entry(filter.enable()).or_default() += 1;
                            }
                            None => *self.filters.entry(-1).or_default() += 1,
                        }
                        match target {
                            Some(id) => {
                                *self
                                    .fade_kinds
                                    .entry(kind_of(participants, id))
                                    .or_default() += 1;
                                spans.entry(id).or_default().push((
                                    f32::from(command.time()),
                                    length.max(0) as f32,
                                ));
                            }
                            None => self.unaddressed += 1,
                        }
                    }
                    CommandKind::C019(_) => {
                        self.shows += 1;
                        match target {
                            Some(id) => {
                                *self
                                    .shown_kinds
                                    .entry(kind_of(participants, id))
                                    .or_default() += 1;
                            }
                            None => self.unaddressed_shows += 1,
                        }
                    }
                    CommandKind::C049(_) => {
                        self.effects += 1;
                        if target.is_none() {
                            self.unaddressed_effects += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        for held in spans.values_mut() {
            if held.len() < 2 {
                continue;
            }
            self.stacked += 1;
            held.sort_by(|a, b| a.0.total_cmp(&b.0));
            if held
                .windows(2)
                .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
            {
                self.overlapping += 1;
            }
        }

        for node in file.nodes() {
            let Node::Unknown(unknown) = node else {
                continue;
            };
            if unknown.magic() != *b"CTCB" {
                continue;
            }
            self.ctcb += 1;
            let body = unknown.body();
            // The first record's own offset reaches the end of the table, which is how many it holds.
            let count = body
                .get(16..20)
                .map(|held| i32::from_le_bytes(held.try_into().unwrap()) as usize / 24)
                .unwrap_or(0);
            self.ctcb_matches += usize::from(count == timelines.len());
            *self.ctcb_over.entry(count as i64 - timelines.len() as i64).or_default() += 1;
            let mut columns: [Vec<i32>; 6] = Default::default();
            for index in 0..count {
                let at = 16 + index * 24;
                if at + 24 > body.len() {
                    break;
                }
                for (lane, column) in columns.iter_mut().enumerate() {
                    let held = at + lane * 4;
                    column.push(i32::from_le_bytes(body[held..held + 4].try_into().unwrap()));
                }
            }
            // What each record's own offset reaches, four words in.
            let mut linked = Vec::new();
            for index in 0..count {
                let at = 16 + index * 24;
                let Some(field) = body.get(at..at + 4) else {
                    break;
                };
                let to = at as i64 + i32::from_le_bytes(field.try_into().unwrap()) as i64;
                let held = usize::try_from(to + 12).ok().and_then(|at| body.get(at..at + 4));
                if let Some(held) = held {
                    linked.push(i32::from_le_bytes(held.try_into().unwrap()));
                }
            }
            let held: BTreeSet<i32> = linked.iter().copied().collect();
            self.link_permutations += usize::from(
                held.len() == linked.len()
                    && !linked.is_empty()
                    && held == (0..linked.len() as i32).collect::<BTreeSet<_>>(),
            );
            self.link_of_timelines += usize::from(
                held.len() == linked.len()
                    && !linked.is_empty()
                    && held == (0..timelines.len() as i32).collect::<BTreeSet<_>>(),
            );
            for (lane, column) in columns.iter().enumerate() {
                for value in column {
                    *self.ctcb_fields[lane].entry(*value).or_default() += 1;
                }
                let held: BTreeSet<i32> = column.iter().copied().collect();
                let ordered = (0..column.len() as i32).collect::<BTreeSet<_>>();
                self.permutations[lane] += usize::from(held == ordered && !column.is_empty());
            }
        }
    }

    fn report(&self) {
        println!("{} cutscenes read", self.read);
        println!(
            "C094: {} fades, {} out, {} in, {} partway",
            self.fades, self.out, self.into, self.part
        );
        println!("  {} state no length at all", self.instant);
        println!("  starts {:?}", self.starts);
        println!("  ends {:?}", self.ends);
        println!(
            "  {} state a filter; filters {:?} (-1 is none); enables {:?}",
            self.filtered, self.filters, self.enables
        );
        println!("  {} reach no participant", self.unaddressed);
        println!("  kinds {:?}", self.fade_kinds);
        println!(
            "  {} participants carry more than one, {} of those overlap",
            self.stacked, self.overlapping
        );
        let lengths: Vec<_> = self.lengths.iter().take(16).collect();
        println!("  lengths {lengths:?}");
        println!(
            "C019: {} commands, {} reach no participant",
            self.shows, self.unaddressed_shows
        );
        println!("  kinds {:?}", self.shown_kinds);
        println!(
            "C049: {} effects, {} reach no participant",
            self.effects, self.unaddressed_effects
        );
        println!(
            "CTCB: {} nodes, {} whose count matches the timelines",
            self.ctcb, self.ctcb_matches
        );
        println!("  permutations of 0..n by lane: {:?}", self.permutations);
        println!("  records over timelines {:?}", self.ctcb_over);
        println!(
            "  {} link lists permute their own records, {} permute the timelines",
            self.link_permutations, self.link_of_timelines
        );
        for (lane, held) in self.ctcb_fields.iter().enumerate() {
            let mut top: Vec<_> = held.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1));
            println!(
                "  lane {lane}: {} distinct, top {:?}",
                held.len(),
                top.iter().take(6).collect::<Vec<_>>()
            );
        }
    }
}
