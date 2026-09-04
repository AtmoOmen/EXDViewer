//! What the visual-effect and visibility commands of every shipping cutscene state: the `.avfx`
//! each `C049` names, the curve set beside it, what a `C019` puts a node into, and whether the two
//! large unmodelled kinds hold anything that reads.
//!
//! `cutb_vfx <paths file>`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::layer::{HelperKind, InstanceData};
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

    effects: usize,
    /// Effects whose path resolves, and how many of those end `.avfx`.
    pathed: [usize; 2],
    /// Effects a `TMAC` addresses, and how many of those name a `CTAL` participant.
    addressed: [usize; 2],
    /// Effects whose curve set is in the same timeline, and how many of those hold exactly the
    /// five channels the clip reads.
    curved: [usize; 2],
    /// Values those five channels ever hold, by channel.
    channels: BTreeMap<u8, BTreeSet<u32>>,
    /// Effects whose five channels are one key of one apiece, how many move at all, and how many
    /// carry them on the target the clip reads.
    white: usize,
    moving: usize,
    zeroth: usize,
    /// Every other field of the body, by name.
    fields: BTreeMap<&'static str, BTreeMap<i64, usize>>,
    /// Paths, and the directory each sits under.
    paths: BTreeSet<String>,
    /// How many of one file's effects share a path, at most.
    shared: BTreeMap<usize, usize>,
    /// The closest two firings of one path in one file ever start, in frames.
    closest: Option<(f32, String)>,

    visibility: usize,
    /// What a `C019` sets, by value.
    states: BTreeMap<i32, usize>,
    /// Participants a file gives more than one `C019`, and pairs of them whose states are
    /// complementary at the same frame.
    swapped: usize,
    pairs: usize,
    /// What a `C019` runs against, by the kind the participant stands for.
    kinds: BTreeMap<String, usize>,

    /// The bodies of the two large unmodelled kinds, and whether a dword of one reaches a string.
    unmodelled: BTreeMap<String, (usize, BTreeMap<usize, usize>)>,
}

/// The participant each of a timeline's commands runs against, out of the actors it drives.
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

fn span(timeline: &Timeline) -> f32 {
    timeline
        .items()
        .iter()
        .find_map(|item| match item {
            Item::Header(header) => Some(f32::from(header.duration())),
            _ => None,
        })
        .unwrap_or(0.0)
        .max(1.0)
}

impl Census {
    fn file(&mut self, path: &str, cutscene: &Cutscene) {
        let kind_of: BTreeMap<u32, String> = cutscene
            .nodes()
            .iter()
            .filter_map(|node| match node {
                Node::Participants(held) => Some(held),
                _ => None,
            })
            .flatten()
            .map(|instance| {
                let named = match instance.data() {
                    InstanceData::HelperObject(helper) => match helper.kind() {
                        HelperKind::EventNpc
                        | HelperKind::BattleNpc
                        | HelperKind::Player
                        | HelperKind::PartyMember
                        | HelperKind::PartyMemberAlt
                        | HelperKind::StableChocobo
                        | HelperKind::Unknown82 => "a character".to_owned(),
                        kind => format!("{kind:?}"),
                    },
                    _ => format!("{:?}", instance.kind()),
                };
                (instance.id(), named)
            })
            .collect();
        let participants: BTreeSet<u32> = cutscene
            .nodes()
            .iter()
            .filter_map(|node| match node {
                Node::Participants(held) => Some(held),
                _ => None,
            })
            .flatten()
            .map(|instance| instance.id())
            .collect();

        // Every effect this file fires, by path, and every visibility change, by participant.
        let mut fired: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut states: BTreeMap<u32, Vec<(f32, i32)>> = BTreeMap::new();
        let mut offset = 0.0;

        for node in cutscene.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            let addressed = addressed(timeline);
            let curves: BTreeMap<i16, Vec<(u8, u32, usize, u8)>> = timeline
                .items()
                .iter()
                .filter_map(|item| match item {
                    Item::Curves(held) => Some((
                        held.id(),
                        held.curves()
                            .iter()
                            .map(|curve| {
                                let value = curve
                                    .keys()
                                    .iter()
                                    .map(|key| key.value().to_bits())
                                    .next()
                                    .unwrap_or_default();
                                (curve.tag() & 0x3F, value, curve.keys().len(), curve.target())
                            })
                            .collect(),
                    )),
                    _ => None,
                })
                .collect();

            for item in timeline.items() {
                let Item::Command(command) = item else {
                    continue;
                };
                let at = offset + f32::from(command.time());
                let participant = addressed.get(&command.id());
                match command.kind() {
                    CommandKind::C049(effect) => {
                        self.effects += 1;
                        if let Some(held) = effect.path() {
                            self.pathed[0] += 1;
                            self.pathed[1] += usize::from(held.ends_with(".avfx"));
                            self.paths.insert(held.to_owned());
                            fired.entry(held.to_owned()).or_default().push(at);
                        }
                        self.addressed[0] += usize::from(participant.is_some());
                        self.addressed[1] += usize::from(
                            participant.is_some_and(|held| participants.contains(held)),
                        );
                        if let Ok(id) = i16::try_from(effect.curve_id())
                            && let Some(held) = curves.get(&id)
                        {
                            self.curved[0] += 1;
                            let channels: BTreeSet<u8> =
                                held.iter().map(|(tag, ..)| *tag).collect();
                            self.curved[1] +=
                                usize::from((0x0a..=0x0e).all(|tag| channels.contains(&tag)));
                            let five = held.iter().filter(|(tag, ..)| (0x0a..=0x0e).contains(tag));
                            self.white += usize::from(
                                five.clone().all(|(_, value, keys, _)| {
                                    *keys == 1 && f32::from_bits(*value) == 1.0
                                }),
                            );
                            self.moving +=
                                usize::from(five.clone().any(|(_, _, keys, _)| *keys > 1));
                            self.zeroth +=
                                usize::from(five.clone().all(|(.., target)| *target == 0));
                            for (tag, value, ..) in five {
                                self.channels.entry(*tag).or_default().insert(*value);
                            }
                        }
                        for (name, value) in [
                            ("enabled", i64::from(effect.enabled())),
                            ("second_object", i64::from(effect.second_object())),
                            ("unknown_2", i64::from(effect.unknown_2())),
                            ("bind_type_1", i64::from(effect.bind_type_1())),
                            ("bind_type_2", i64::from(effect.bind_type_2())),
                            ("unknown_3", i64::from(effect.unknown_3())),
                            ("bind_id_1", i64::from(effect.bind_id_1())),
                            ("bind_id_2", i64::from(effect.bind_id_2())),
                            ("unknown_4", i64::from(effect.unknown_4())),
                            ("flags", i64::from(effect.flags())),
                            ("unknown_5", i64::from(effect.unknown_5())),
                            ("unknown_6", i64::from(effect.unknown_6())),
                            ("unknown_7", i64::from(effect.unknown_7())),
                        ] {
                            *self.fields.entry(name).or_default().entry(value).or_default() += 1;
                        }
                    }
                    CommandKind::C019(held) => {
                        self.visibility += 1;
                        *self.states.entry(held.visibility()).or_default() += 1;
                        if let Some(participant) = participant {
                            states.entry(*participant).or_default().push((at, held.visibility()));
                            let named = kind_of
                                .get(participant)
                                .cloned()
                                .unwrap_or_else(|| "no participant".to_owned());
                            *self.kinds.entry(named).or_default() += 1;
                        }
                    }
                    CommandKind::Unknown { magic, body } => {
                        let magic = String::from_utf8_lossy(magic).into_owned();
                        if magic != "C128" && magic != "C156" {
                            continue;
                        }
                        let entry = self.unmodelled.entry(magic).or_default();
                        entry.0 += 1;
                        for (index, word) in body.chunks_exact(4).enumerate() {
                            let word = i32::from_le_bytes(word.try_into().unwrap());
                            if word != 0 {
                                *entry.1.entry(index).or_default() += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            offset += span(timeline);
        }

        for (effect, mut times) in fired {
            *self.shared.entry(times.len()).or_default() += 1;
            times.sort_by(f32::total_cmp);
            for gap in times.windows(2).map(|held| held[1] - held[0]) {
                if self.closest.as_ref().is_none_or(|(held, _)| gap < *held) {
                    self.closest = Some((gap, format!("{effect} in {path}")));
                }
            }
        }

        // A pair swapping is two participants whose states move the opposite way at one frame.
        let mut at_frame: BTreeMap<u32, Vec<(u32, i32)>> = BTreeMap::new();
        for (participant, held) in &states {
            if held.len() > 1 {
                self.swapped += 1;
            }
            for (at, state) in held {
                at_frame.entry(at.to_bits()).or_default().push((*participant, *state));
            }
        }
        for held in at_frame.values() {
            let shown = held.iter().filter(|(_, state)| state & 0xFF != 0).count();
            let hidden = held.len() - shown;
            self.pairs += shown.min(hidden);
        }
    }

    fn report(&self) {
        println!("{} read, {} failed\n", self.read, self.failed);

        println!("{} C049", self.effects);
        println!(
            "  {} naming a path, {} of them a .avfx, {} distinct",
            self.pathed[0],
            self.pathed[1],
            self.paths.len()
        );
        println!(
            "  {} a track addresses, {} of those naming a participant the same file holds",
            self.addressed[0], self.addressed[1]
        );
        println!(
            "  {} naming a curve set beside them, {} of those holding channels 0x0a..0x0e",
            self.curved[0], self.curved[1]
        );
        println!(
            "  {} whose five channels are one key of one apiece, {} holding a channel that moves",
            self.white, self.moving
        );
        println!("  {} carrying those channels on target 0", self.zeroth);
        for (tag, values) in &self.channels {
            let held: Vec<String> = values
                .iter()
                .take(6)
                .map(|bits| format!("{}", f32::from_bits(*bits)))
                .collect();
            println!(
                "    channel {tag:#04x}: {} distinct, {}",
                values.len(),
                held.join(", ")
            );
        }
        for (name, values) in &self.fields {
            let held: Vec<String> = values
                .iter()
                .take(6)
                .map(|(value, count)| format!("{value}: {count}"))
                .collect();
            println!(
                "  {name:>14}  {} distinct, {}",
                values.len(),
                held.join(", ")
            );
        }
        println!("\n  how many of one file's C049 share a path");
        for (count, files) in &self.shared {
            println!("    {count:>3} firings: {files}");
        }
        if let Some((gap, where_)) = &self.closest {
            println!("    closest two firings of one path: {gap} frames, {where_}");
        }

        println!("\n{} C019, by what they state", self.visibility);
        for (state, count) in &self.states {
            println!("  {state:#06x}  {count}");
        }
        println!(
            "  {} participants a file states more than once, {} of them opposite another at the \
             same frame",
            self.swapped, self.pairs
        );
        for (named, count) in &self.kinds {
            println!("  {named:>16}  {count}");
        }

        println!("\nthe two large unmodelled kinds, and which dwords of the body are ever set");
        for (magic, (count, words)) in &self.unmodelled {
            let held: Vec<String> = words
                .iter()
                .map(|(index, count)| format!("{index}: {count}"))
                .collect();
            println!("  {magic}  {count}  {}", held.join(", "));
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a paths file");
    let paths = std::fs::read_to_string(list).expect("the paths file");

    let mut census = Census::default();
    for path in paths.lines().map(str::trim).filter(|path| !path.is_empty()) {
        match ironworks.file::<Cutscene>(path) {
            Ok(cutscene) => {
                census.read += 1;
                census.file(path, &cutscene);
            }
            Err(_) => census.failed += 1,
        }
    }
    census.report();
}
