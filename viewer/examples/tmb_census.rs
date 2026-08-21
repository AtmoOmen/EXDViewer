//! Every `Cxxx` a timeline holds, wherever the game embeds a timeline, and what its body's dwords
//! ever hold.
//!
//! `tmb_census <magics|fields|dump|tracks|ticks> [magic] [limit]`

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ironworks::file::layer::InstanceData;
use ironworks::file::{File, cutb::Cutscene, pap::AnimationPack, sgb::SharedGroupFile, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

/// What a timeline was reached through, since a scene's vocabulary need not be a character's.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Source {
    Sgb,
    Tmb,
    Pap,
    Cutb,
}

/// Everything one timeline states that a command body's dword might be naming.
#[derive(Default)]
struct Names {
    curves: BTreeSet<i64>,
    actors: BTreeSet<i64>,
    actor_times: BTreeSet<i64>,
    tracks: BTreeSet<i64>,
    commands: BTreeSet<i64>,
    instances: BTreeSet<i64>,
    animated: BTreeSet<i64>,
    duration: i64,
}

#[derive(Default)]
struct Slot {
    values: BTreeMap<i64, usize>,
    zero: usize,
    curve: usize,
    actor: usize,
    actor_time: usize,
    track: usize,
    command: usize,
    instance: usize,
    animated: usize,
    duration: usize,
    string: usize,
    float: usize,
    negative: usize,
    small: usize,
}

#[derive(Default)]
struct Shape {
    bodies: usize,
    lengths: BTreeMap<usize, usize>,
    slots: Vec<Slot>,
    files: BTreeSet<String>,
    times: BTreeSet<i16>,
}

fn hit(body: &[u8], at: i64) -> Option<String> {
    // A command reaches its heap from `item_start + 8`, four bytes ahead of where its body starts.
    let start = usize::try_from(at - 4).ok()?;
    let rest = body.get(start..)?;
    let end = rest.iter().position(|byte| *byte == 0)?;
    let text = rest.get(..end)?;
    match text.len() >= 3 && text.iter().all(|byte| byte.is_ascii_graphic()) {
        true => Some(String::from_utf8_lossy(text).into_owned()),
        false => None,
    }
}

fn names(items: &[tmb::Item], scene: &BTreeSet<i64>, animated: &BTreeSet<i64>) -> Names {
    let mut held = Names {
        instances: scene.clone(),
        animated: animated.clone(),
        ..Default::default()
    };
    for item in items {
        match item {
            tmb::Item::Curves(curves) => {
                held.curves.insert(curves.id().into());
            }
            tmb::Item::Actor(actor) => {
                held.actors.insert(actor.id().into());
                held.actor_times.insert(actor.time().into());
            }
            tmb::Item::Track(track) => {
                held.tracks.insert(track.id().into());
            }
            tmb::Item::Command(command) => {
                held.commands.insert(command.id().into());
            }
            tmb::Item::Header(header) => held.duration = header.duration().into(),
            _ => (),
        }
    }
    held
}

fn magic_of(command: &tmb::Command) -> String {
    match command.kind() {
        tmb::CommandKind::Unknown { magic, .. } => String::from_utf8_lossy(magic).into_owned(),
        held => format!("{held:?}")
            .split(['(', ' '])
            .next()
            .unwrap()
            .to_owned(),
    }
}

/// The body of a command, which the reader keeps only for a magic it does not model.
fn body_of(command: &tmb::Command) -> Option<&[u8]> {
    match command.kind() {
        tmb::CommandKind::Unknown { body, .. } => Some(body),
        _ => None,
    }
}

struct Census {
    mode: String,
    wanted: String,
    limit: usize,
    counts: BTreeMap<(Source, bool, String), usize>,
    shapes: BTreeMap<(Source, String), Shape>,
    dumped: usize,
    /// Magics sharing one `TMTR` with the wanted magic, and how often the wanted one stands alone.
    together: BTreeMap<String, usize>,
    alone: usize,
    tracks: usize,
    /// How an actor list reaches the actors of a scene timeline.
    reach: [usize; 3],
    files: usize,
    failed: usize,
    timelines: usize,
    /// What the instance an actor drives turns out to be, per command magic.
    kinds: BTreeMap<String, BTreeMap<String, usize>>,
    named: BTreeMap<String, BTreeMap<String, usize>>,
    /// Where each value of the second dword was seen.
    ids: HashMap<u32, (usize, BTreeSet<String>)>,
    era: BTreeMap<String, (u32, u32, usize)>,
}

impl Census {
    fn timeline(
        &mut self,
        source: Source,
        path: &str,
        timeline: &tmb::Timeline,
        scene: &BTreeSet<i64>,
        animated: &BTreeSet<i64>,
    ) {
        self.timelines += 1;
        let items = timeline.items();
        let layout = timeline.layout();
        let held = names(items, scene, animated);

        for item in items {
            let tmb::Item::Command(command) = item else {
                continue;
            };
            let magic = magic_of(command);
            *self
                .counts
                .entry((source, layout == tmb::Layout::Wide, magic.clone()))
                .or_default() += 1;
            if layout == tmb::Layout::Wide {
                continue;
            }
            if !self.wanted.is_empty() && magic != self.wanted {
                continue;
            }
            let Some(body) = body_of(command) else {
                continue;
            };
            if self.mode == "dump" && self.dumped < self.limit {
                self.dumped += 1;
                let dwords: Vec<String> = body
                    .chunks_exact(4)
                    .map(|dword| i32::from_le_bytes(dword.try_into().unwrap()).to_string())
                    .collect();
                println!(
                    "{path} id {} time {} [{}] tail {:?}",
                    command.id(),
                    command.time(),
                    dwords.join(" "),
                    &body[body.len() - body.len() % 4..]
                );
            }

            if let Some(dword) = body.get(4..8) {
                let held = u32::from_le_bytes(dword.try_into().unwrap());
                let seen = self.ids.entry(held).or_default();
                seen.0 += 1;
                if seen.1.len() < 8 {
                    seen.1.insert(path.to_owned());
                }
                let era = path
                    .split('/')
                    .nth(1)
                    .unwrap_or("?")
                    .to_owned();
                let band = self.era.entry(era).or_insert((u32::MAX, 0, 0));
                band.0 = band.0.min(held);
                band.1 = band.1.max(held);
                band.2 += 1;
            }

            let shape = self.shapes.entry((source, magic)).or_default();
            shape.bodies += 1;
            *shape.lengths.entry(body.len()).or_default() += 1;
            shape.files.insert(path.to_owned());
            shape.times.insert(command.time());
            for (at, dword) in body.chunks_exact(4).enumerate() {
                let value = i64::from(i32::from_le_bytes(dword.try_into().unwrap()));
                if shape.slots.len() <= at {
                    shape.slots.resize_with(at + 1, Slot::default);
                }
                let slot = &mut shape.slots[at];
                if slot.values.len() < 4096 {
                    *slot.values.entry(value).or_default() += 1;
                }
                slot.zero += usize::from(value == 0);
                slot.negative += usize::from(value < 0);
                slot.small += usize::from((1..=64).contains(&value));
                slot.curve += usize::from(held.curves.contains(&value));
                slot.actor += usize::from(held.actors.contains(&value));
                slot.actor_time += usize::from(held.actor_times.contains(&value));
                slot.track += usize::from(held.tracks.contains(&value));
                slot.command += usize::from(held.commands.contains(&value));
                slot.instance += usize::from(held.instances.contains(&value));
                slot.animated += usize::from(held.animated.contains(&value));
                slot.duration += usize::from(value != 0 && value == held.duration);
                slot.string += usize::from(hit(body, value).is_some());
                let float = f32::from_bits(value as u32);
                slot.float += usize::from(
                    float != 0.0 && float.is_finite() && (1e-3..1e6).contains(&float.abs()),
                );
            }
        }

        if self.mode == "tracks" && !self.wanted.is_empty() && layout == tmb::Layout::Standard {
            let magics: HashMap<i16, String> = items
                .iter()
                .filter_map(|item| match item {
                    tmb::Item::Command(command) => Some((command.id(), magic_of(command))),
                    _ => None,
                })
                .collect();
            for item in items {
                let tmb::Item::Track(track) = item else {
                    continue;
                };
                let held: Vec<&String> = track
                    .commands()
                    .iter()
                    .filter_map(|id| magics.get(id))
                    .collect();
                if !held.iter().any(|magic| **magic == self.wanted) {
                    continue;
                }
                self.tracks += 1;
                let others: BTreeSet<&str> = held
                    .iter()
                    .map(|magic| magic.as_str())
                    .filter(|magic| *magic != self.wanted)
                    .collect();
                if others.is_empty() {
                    self.alone += 1;
                }
                for magic in others {
                    *self.together.entry(magic.to_owned()).or_default() += 1;
                }
            }
        }
    }
}

fn scene_instances(file: &SharedGroupFile) -> (BTreeSet<i64>, Vec<String>) {
    let mut instances = BTreeSet::new();
    let mut children = Vec::new();
    for group in file.scene().layer_groups() {
        for layer in group.layers() {
            for instance in layer.instances() {
                instances.insert(instance.id().into());
                if let InstanceData::SharedGroup(child) = instance.data()
                    && !child.asset_path().is_empty()
                {
                    children.push(child.asset_path().clone());
                }
            }
        }
    }
    (instances, children)
}

/// What each instance of a scene is, and what it was named.
fn scene_kinds(file: &SharedGroupFile) -> BTreeMap<i64, (String, String)> {
    let mut kinds = BTreeMap::new();
    for group in file.scene().layer_groups() {
        for layer in group.layers() {
            for instance in layer.instances() {
                let what = match instance.data() {
                    InstanceData::BgPart(held) => format!("BgPart {}", held.asset_path()),
                    InstanceData::Vfx(held) => format!("Vfx {}", held.asset_path()),
                    InstanceData::Sound(held) => format!("Sound {}", held.asset_path()),
                    InstanceData::SharedGroup(held) => format!("SharedGroup {}", held.asset_path()),
                    held => format!("{held:?}").split(['(', ' ']).next().unwrap().to_owned(),
                };
                kinds.insert(instance.id().into(), (what, instance.name().clone()));
            }
        }
    }
    kinds
}

/// What an instance states about itself that a command driving it might be restating.
fn instance_note(instance: &ironworks::file::layer::Instance) -> String {
    let placed = instance.transform();
    let mut note = format!(
        "at {:?} turn {:?} size {:?}",
        placed.translation(),
        placed.rotation(),
        placed.scale()
    );
    match instance.data() {
        InstanceData::Light(held) => {
            let colour = held.colour();
            note += &format!(
                " colour {} {} {} {} x{}",
                colour.red(),
                colour.green(),
                colour.blue(),
                colour.alpha(),
                colour.intensity()
            );
        }
        InstanceData::BgPart(held) => note += &format!(" model {}", held.asset_path()),
        InstanceData::Vfx(held) => note += &format!(" vfx {}", held.asset_path()),
        InstanceData::Sound(held) => note += &format!(" sound {}", held.asset_path()),
        _ => (),
    }
    note
}

/// Which commands one actor of a timeline runs.
fn actor_commands<'a>(items: &'a [tmb::Item], actor: i32) -> Vec<&'a tmb::Command> {
    let Some(tracks) = items.iter().find_map(|item| match item {
        tmb::Item::Actor(held) if i32::from(held.time()) == actor => Some(held.tracks()),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut held = Vec::new();
    for track in tracks {
        let Some(commands) = items.iter().find_map(|item| match item {
            tmb::Item::Track(found) if found.id() == *track => Some(found.commands()),
            _ => None,
        }) else {
            continue;
        };
        for command in commands {
            if let Some(tmb::Item::Command(found)) = items
                .iter()
                .find(|item| matches!(item, tmb::Item::Command(found) if found.id() == *command))
            {
                held.push(found);
            }
        }
    }
    held
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mode = std::env::args().nth(1).unwrap_or_else(|| "magics".into());
    let wanted = std::env::args().nth(2).unwrap_or_default();
    let limit = std::env::args()
        .nth(3)
        .and_then(|held| held.parse().ok())
        .unwrap_or(usize::MAX);

    let mut census = Census {
        mode: mode.clone(),
        wanted,
        limit: limit.min(64),
        counts: BTreeMap::new(),
        shapes: BTreeMap::new(),
        dumped: 0,
        together: BTreeMap::new(),
        alone: 0,
        tracks: 0,
        reach: [0; 3],
        files: 0,
        failed: 0,
        timelines: 0,
        kinds: BTreeMap::new(),
        named: BTreeMap::new(),
        ids: HashMap::new(),
        era: BTreeMap::new(),
    };

    let listed = std::fs::read_to_string(PATHS).expect("the path list");
    let mut sgb: BTreeSet<&str> = BTreeSet::new();
    let mut rest: Vec<(Source, &str)> = Vec::new();
    for path in listed.lines() {
        match path.rsplit('.').next() {
            Some("sgb") => {
                sgb.insert(path);
            }
            Some("tmb") => rest.push((Source::Tmb, path)),
            Some("pap") => rest.push((Source::Pap, path)),
            Some("cutb") => rest.push((Source::Cutb, path)),
            _ => (),
        }
    }

    // A path list does not name every shared group, so the ones a group holds are followed too.
    let mut every: BTreeSet<String> = sgb.iter().map(|held| (*held).to_owned()).collect();
    let mut wave: Vec<String> = every.iter().cloned().collect();
    while !wave.is_empty() {
        let mut next = Vec::new();
        for path in &wave {
            let Ok(file) = ironworks.file::<SharedGroupFile>(path.as_str()) else {
                continue;
            };
            for child in scene_instances(&file).1 {
                if every.insert(child.clone()) {
                    next.push(child);
                }
            }
        }
        wave = next;
    }

    for path in &every {
        let Ok(file) = ironworks.file::<SharedGroupFile>(path.as_str()) else {
            census.failed += 1;
            continue;
        };
        census.files += 1;
        let (instances, _) = scene_instances(&file);
        if mode == "show" && census.dumped < census.limit {
            let mut placed = BTreeMap::new();
            for group in file.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        placed.insert(i64::from(instance.id()), instance);
                    }
                }
            }
            for timeline in file.scene().timelines() {
                for (actor, instance) in timeline.animated() {
                    for command in actor_commands(timeline.timeline().items(), *actor) {
                        if magic_of(command) != census.wanted || census.dumped >= census.limit {
                            continue;
                        }
                        let Some(body) = body_of(command) else {
                            continue;
                        };
                        census.dumped += 1;
                        let ints: Vec<String> = body
                            .chunks_exact(4)
                            .map(|dword| i32::from_le_bytes(dword.try_into().unwrap()).to_string())
                            .collect();
                        let floats: Vec<String> = body
                            .chunks_exact(4)
                            .map(|dword| {
                                format!("{:.4}", f32::from_le_bytes(dword.try_into().unwrap()))
                            })
                            .collect();
                        println!("{path}");
                        println!(
                            "   time {} auto {} [{}]",
                            command.time(),
                            timeline.auto_play(),
                            ints.join(" ")
                        );
                        println!("   as floats [{}]", floats.join(" "));
                        match placed.get(&i64::from(*instance)) {
                            Some(held) => println!(
                                "   drives #{instance} {:?} {:?} {}",
                                held.kind(),
                                held.name(),
                                instance_note(held)
                            ),
                            None => println!("   drives #{instance}, absent from this scene"),
                        }
                    }
                }
            }
        }
        if mode == "peractor" {
            for timeline in file.scene().timelines() {
                for (actor, _) in timeline.animated() {
                    let mut held: BTreeMap<String, usize> = BTreeMap::new();
                    for command in actor_commands(timeline.timeline().items(), *actor) {
                        *held.entry(magic_of(command)).or_default() += 1;
                        let magic = magic_of(command);
                        *census
                            .named
                            .entry(magic)
                            .or_default()
                            .entry(match command.time() {
                                0 => "at time zero".to_owned(),
                                _ => "later".to_owned(),
                            })
                            .or_default() += 1;
                    }
                    let with: BTreeSet<&String> = held.keys().collect();
                    for (magic, count) in &held {
                        let per = census.kinds.entry(magic.clone()).or_default();
                        *per.entry(match count {
                            1 => "one per actor".to_owned(),
                            _ => "several per actor".to_owned(),
                        })
                        .or_default() += 1;
                        *per.entry(match with.contains(&"C013".to_owned()) {
                            true => "beside C013".to_owned(),
                            false => "no C013".to_owned(),
                        })
                        .or_default() += 1;
                    }
                }
            }
        }
        if mode == "states" {
            let held = file.scene().timelines();
            let playing = held.iter().filter(|timeline| timeline.auto_play()).count();
            let mut driven: BTreeMap<i32, usize> = BTreeMap::new();
            for timeline in held {
                *census
                    .kinds
                    .entry("timeline kind".to_owned())
                    .or_default()
                    .entry(format!(
                        "{:?} auto {} loop {}",
                        timeline.kind(),
                        timeline.auto_play(),
                        timeline.looping()
                    ))
                    .or_default() += 1;
                if timeline.auto_play() {
                    for (_, instance) in timeline.animated() {
                        *driven.entry(*instance).or_default() += 1;
                    }
                }
            }
            *census
                .kinds
                .entry("timelines a scene holds".to_owned())
                .or_default()
                .entry(format!("{} of them {playing} playing themselves", held.len()))
                .or_default() += 1;
            for count in driven.values() {
                *census
                    .kinds
                    .entry("instances driven by".to_owned())
                    .or_default()
                    .entry(format!("{count} playing timelines"))
                    .or_default() += 1;
            }
        }
        if mode == "check" {
            let mut placed = BTreeMap::new();
            for group in file.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        placed.insert(i64::from(instance.id()), instance);
                    }
                }
            }
            for timeline in file.scene().timelines() {
                for (actor, instance) in timeline.animated() {
                    let Some(held) = placed.get(&i64::from(*instance)) else {
                        continue;
                    };
                    let every = actor_commands(timeline.timeline().items(), *actor);
                    let curved = every
                        .iter()
                        .any(|command| matches!(command.kind(), tmb::CommandKind::C013(_)));
                    for command in &every {
                        let mut tally = |what: &str, how: String| {
                            *census
                                .kinds
                                .entry(what.to_owned())
                                .or_default()
                                .entry(how)
                                .or_default() += 1;
                        };
                        match command.kind() {
                            tmb::CommandKind::C018(found) => {
                                let stated = held.transform();
                                let gap = (0..3)
                                    .map(|axis| {
                                        (stated.translation()[axis] - found.translation()[axis])
                                            .abs()
                                    })
                                    .fold(0.0f32, f32::max);
                                let turn = (0..3)
                                    .map(|axis| {
                                        (stated.rotation()[axis] - found.rotation()[axis]).abs()
                                    })
                                    .fold(0.0f32, f32::max);
                                if !timeline.auto_play() || curved {
                                    continue;
                                }
                                tally(
                                    "C018 that plays itself, at time zero",
                                    format!("{}", command.time() == 0),
                                );
                                if command.time() != 0 {
                                    continue;
                                }
                                if gap > 10.0 && census.dumped < 24 {
                                    census.dumped += 1;
                                    println!(
                                        "   {path} #{instance} {:?} {:?} from {:?} to {:?}",
                                        held.kind(),
                                        held.name(),
                                        stated.translation(),
                                        found.translation()
                                    );
                                }
                                tally(
                                    "C018 in a loop named",
                                    format!("{:?}", timeline.kind()),
                                );
                                tally(
                                    "C018 moved by",
                                    match gap {
                                        _ if gap == 0.0 => "nothing",
                                        _ if gap < 0.001 => "under a millimetre",
                                        _ if gap < 0.1 => "under a decimetre",
                                        _ if gap < 1.0 => "under a metre",
                                        _ if gap < 10.0 => "under ten metres",
                                        _ => "more than ten metres",
                                    }
                                    .to_owned(),
                                );
                                tally(
                                    "C018 turned by",
                                    match turn {
                                        _ if turn == 0.0 => "nothing",
                                        _ if turn < 0.001 => "under a milliradian",
                                        _ if turn < 0.1 => "under a tenth",
                                        _ if turn < 1.0 => "under a radian",
                                        _ => "more than a radian",
                                    }
                                    .to_owned(),
                                );
                                tally(
                                    "C018 in a timeline",
                                    match timeline.auto_play() {
                                        true => "that plays itself",
                                        false => "waiting to be played",
                                    }
                                    .to_owned(),
                                );
                                tally(
                                    "C018 scaled to",
                                    format!("{:?}", found.scale()),
                                );
                            }
                            tmb::CommandKind::C112(found) => {
                                let InstanceData::Light(light) = held.data() else {
                                    continue;
                                };
                                let colour = light.colour();
                                let stated = [colour.red(), colour.green(), colour.blue()]
                                    .map(|held| f32::from(held) / 255.0);
                                let found = found.color();
                                let reach = found
                                    .iter()
                                    .zip(stated)
                                    .find(|(_, stated)| *stated > 0.05)
                                    .map(|(found, stated)| found / stated)
                                    .unwrap_or(0.0);
                                let same = (0..3)
                                    .all(|axis| (found[axis] - stated[axis] * reach).abs() < 0.02);
                                tally(
                                    "C112 colour",
                                    match (reach == 0.0, same) {
                                        (true, _) => "black".to_owned(),
                                        (_, true) => format!(
                                            "the light's own hue times {}",
                                            match (reach - colour.intensity()).abs() < 0.01 {
                                                true => "its own intensity",
                                                false => "another intensity",
                                            }
                                        ),
                                        _ => "a hue of its own".to_owned(),
                                    },
                                );
                            }
                            _ => (),
                        }
                    }
                }
            }
        }
        if mode == "peractor" {
            for timeline in file.scene().timelines() {
                for (actor, _) in timeline.animated() {
                    let mut held: BTreeMap<String, usize> = BTreeMap::new();
                    for command in actor_commands(timeline.timeline().items(), *actor) {
                        *held.entry(magic_of(command)).or_default() += 1;
                        let magic = magic_of(command);
                        *census
                            .named
                            .entry(magic)
                            .or_default()
                            .entry(match command.time() {
                                0 => "at time zero".to_owned(),
                                _ => "later".to_owned(),
                            })
                            .or_default() += 1;
                    }
                    let with: BTreeSet<&String> = held.keys().collect();
                    for (magic, count) in &held {
                        let per = census.kinds.entry(magic.clone()).or_default();
                        *per.entry(match count {
                            1 => "one per actor".to_owned(),
                            _ => "several per actor".to_owned(),
                        })
                        .or_default() += 1;
                        *per.entry(match with.contains(&"C013".to_owned()) {
                            true => "beside C013".to_owned(),
                            false => "no C013".to_owned(),
                        })
                        .or_default() += 1;
                    }
                }
            }
        }
        if mode == "states" {
            let held = file.scene().timelines();
            let playing = held.iter().filter(|timeline| timeline.auto_play()).count();
            let mut driven: BTreeMap<i32, usize> = BTreeMap::new();
            for timeline in held {
                *census
                    .kinds
                    .entry("timeline kind".to_owned())
                    .or_default()
                    .entry(format!(
                        "{:?} auto {} loop {}",
                        timeline.kind(),
                        timeline.auto_play(),
                        timeline.looping()
                    ))
                    .or_default() += 1;
                if timeline.auto_play() {
                    for (_, instance) in timeline.animated() {
                        *driven.entry(*instance).or_default() += 1;
                    }
                }
            }
            *census
                .kinds
                .entry("timelines a scene holds".to_owned())
                .or_default()
                .entry(format!("{} of them {playing} playing themselves", held.len()))
                .or_default() += 1;
            for count in driven.values() {
                *census
                    .kinds
                    .entry("instances driven by".to_owned())
                    .or_default()
                    .entry(format!("{count} playing timelines"))
                    .or_default() += 1;
            }
        }
        if mode == "check" {
            let mut placed = BTreeMap::new();
            for group in file.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        placed.insert(i64::from(instance.id()), instance);
                    }
                }
            }
            for timeline in file.scene().timelines() {
                for (actor, instance) in timeline.animated() {
                    let Some(held) = placed.get(&i64::from(*instance)) else {
                        continue;
                    };
                    for command in actor_commands(timeline.timeline().items(), *actor) {
                        let magic = magic_of(command);
                        let Some(body) = body_of(command) else {
                            continue;
                        };
                        let float = |at: usize| {
                            body.get(at * 4..at * 4 + 4)
                                .map(|dword| f32::from_le_bytes(dword.try_into().unwrap()))
                                .unwrap_or(f32::NAN)
                        };
                        let near = |held: f32, found: f32| {
                            (held - found).abs() <= 1e-3 * held.abs().max(1.0)
                        };
                        let tally = |census: &mut Census, what: &str, how: &str| {
                            *census
                                .kinds
                                .entry(what.to_owned())
                                .or_default()
                                .entry(how.to_owned())
                                .or_default() += 1;
                        };
                        if magic == "C018" && body.len() == 44 {
                            let placed = held.transform();
                            let gap = (0..3)
                                .map(|axis| (placed.translation()[axis] - float(2 + axis)).abs())
                                .fold(0.0f32, f32::max);
                            let turn = (0..3)
                                .map(|axis| (placed.rotation()[axis] - float(5 + axis)).abs())
                                .fold(0.0f32, f32::max);
                            tally(
                                &mut census,
                                "C018 moved by",
                                match gap {
                                    _ if gap == 0.0 => "nothing",
                                    _ if gap < 0.001 => "under a millimetre",
                                    _ if gap < 0.1 => "under a decimetre",
                                    _ if gap < 1.0 => "under a metre",
                                    _ if gap < 10.0 => "under ten metres",
                                    _ => "more than ten metres",
                                },
                            );
                            tally(
                                &mut census,
                                "C018 turned by",
                                match turn {
                                    _ if turn == 0.0 => "nothing",
                                    _ if turn < 0.001 => "under a milliradian",
                                    _ if turn < 0.1 => "under a tenth",
                                    _ if turn < 1.0 => "under a radian",
                                    _ => "more than a radian",
                                },
                            );
                            tally(
                                &mut census,
                                "C018 in a timeline",
                                match timeline.auto_play() {
                                    true => "that plays itself",
                                    false => "waiting to be played",
                                },
                            );
                            for (at, (name, stated)) in [
                                ("translation", placed.translation()),
                                ("rotation", placed.rotation()),
                                ("scale", placed.scale()),
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                let same = (0..3).all(|axis| near(stated[axis], float(2 + at * 3 + axis)));
                                tally(
                                    &mut census,
                                    &format!("C018 {name}"),
                                    match same {
                                        true => "matches the instance",
                                        false => "differs",
                                    },
                                );
                            }
                        }
                        if (magic == "C112" || magic == "C113") && body.len() == 24 {
                            let mut how = "no light".to_owned();
                            if let InstanceData::Light(light) = held.data() {
                                let colour = light.colour();
                                let stated = [colour.red(), colour.green(), colour.blue()]
                                    .map(|held| f32::from(held) / 255.0);
                                let found = [float(2), float(3), float(4)];
                                let scale = found
                                    .iter()
                                    .zip(stated)
                                    .filter(|(_, stated)| *stated > 0.05)
                                    .map(|(found, stated)| found / stated)
                                    .next()
                                    .unwrap_or(0.0);
                                let same = (0..3)
                                    .all(|axis| (found[axis] - stated[axis] * scale).abs() < 0.02);
                                how = match (scale == 0.0, same) {
                                    (true, _) => "black".to_owned(),
                                    (_, true) => format!(
                                        "the instance colour times {}",
                                        match (scale - colour.intensity()).abs() < 0.01 {
                                            true => "its own intensity",
                                            false => "another intensity",
                                        }
                                    ),
                                    _ => "a different hue".to_owned(),
                                };
                            }
                            tally(&mut census, &format!("{magic} colour"), &how);
                        }
                    }
                }
            }
        }
        if mode == "flag" {
            let mut placed = BTreeMap::new();
            for group in file.scene().layer_groups() {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        placed.insert(i64::from(instance.id()), instance);
                    }
                }
            }
            for timeline in file.scene().timelines() {
                for (actor, instance) in timeline.animated() {
                    for command in actor_commands(timeline.timeline().items(), *actor) {
                        if magic_of(command) != census.wanted {
                            continue;
                        }
                        let Some(body) = body_of(command) else {
                            continue;
                        };
                        let Some(dword) = body.get(8..12) else {
                            continue;
                        };
                        let value = i32::from_le_bytes(dword.try_into().unwrap());
                        let what = match placed.get(&i64::from(*instance)).map(|held| held.data()) {
                            Some(InstanceData::BgPart(held)) => {
                                format!("BgPart visible {}", held.visible())
                            }
                            Some(InstanceData::Light(held)) => {
                                format!("Light x{}", held.colour().intensity())
                            }
                            Some(held) => format!("{held:?}").split(['(', ' ']).next().unwrap().to_owned(),
                            None => "absent".to_owned(),
                        };
                        *census
                            .kinds
                            .entry(format!("{} = {value}", census.wanted))
                            .or_default()
                            .entry(what)
                            .or_default() += 1;
                        *census
                            .named
                            .entry(format!("{} = {value}", census.wanted))
                            .or_default()
                            .entry(format!("time {} auto {}", command.time(), timeline.auto_play()))
                            .or_default() += 1;
                    }
                }
            }
        }
        if mode == "kinds" {
            let placed = scene_kinds(&file);
            for timeline in file.scene().timelines() {
                for (actor, instance) in timeline.animated() {
                    let (what, name) = placed
                        .get(&i64::from(*instance))
                        .cloned()
                        .unwrap_or_else(|| ("absent".into(), String::new()));
                    let what = what.split(' ').next().unwrap().to_owned();
                    for command in actor_commands(timeline.timeline().items(), *actor) {
                        let magic = magic_of(command);
                        *census
                            .kinds
                            .entry(magic.clone())
                            .or_default()
                            .entry(what.clone())
                            .or_default() += 1;
                        if !name.is_empty() {
                            *census
                                .named
                                .entry(magic)
                                .or_default()
                                .entry(name.clone())
                                .or_default() += 1;
                        }
                    }
                }
            }
        }
        for timeline in file.scene().timelines() {
            let items = timeline.timeline().items();
            for (actor, instance) in timeline.animated() {
                let by_time = items.iter().any(|item| {
                    matches!(item, tmb::Item::Actor(held) if i32::from(held.time()) == *actor)
                });
                let by_id = items.iter().any(|item| {
                    matches!(item, tmb::Item::Actor(held) if i32::from(held.id()) == *actor)
                });
                census.reach[0] += usize::from(by_time);
                census.reach[1] += usize::from(by_id);
                census.reach[2] += usize::from(!by_time && !by_id);
                let _ = instance;
            }
            let animated = timeline
                .animated()
                .iter()
                .flat_map(|(actor, instance)| [i64::from(*actor), i64::from(*instance)])
                .collect();
            census.timeline(
                Source::Sgb,
                path,
                timeline.timeline(),
                &instances,
                &animated,
            );
        }
    }
    eprintln!("{} shared groups walked", census.files);

    let empty = BTreeSet::new();
    for (source, path) in &rest {
        match source {
            Source::Tmb => match ironworks.file::<tmb::Timeline>(*path) {
                Ok(held) => {
                    census.files += 1;
                    census.timeline(*source, path, &held, &empty, &empty);
                }
                Err(_) => census.failed += 1,
            },
            Source::Pap => match ironworks.file::<AnimationPack>(*path) {
                Ok(pack) => {
                    census.files += 1;
                    for bytes in pack.timelines() {
                        let Ok(held) = tmb::Timeline::read(std::io::Cursor::new(bytes.clone()))
                        else {
                            continue;
                        };
                        census.timeline(*source, path, &held, &empty, &empty);
                    }
                }
                Err(_) => census.failed += 1,
            },
            Source::Cutb => match ironworks.file::<Cutscene>(*path) {
                Ok(cutscene) => {
                    census.files += 1;
                    for node in cutscene.nodes() {
                        if let ironworks::file::cutb::Node::Timeline(held) = node {
                            census.timeline(*source, path, held, &empty, &empty);
                        }
                    }
                }
                Err(_) => census.failed += 1,
            },
            Source::Sgb => (),
        }
    }

    if mode == "ids" || mode == "fields" {
        let repeated: Vec<(&u32, &(usize, BTreeSet<String>))> = census
            .ids
            .iter()
            .filter(|(_, (count, _))| *count > 1)
            .collect();
        let across = repeated
            .iter()
            .filter(|(_, (_, files))| files.len() > 1)
            .count();
        println!(
            "{} distinct second dwords over {} commands; {} repeat, {} of those across files",
            census.ids.len(),
            census.ids.values().map(|(count, _)| count).sum::<usize>(),
            repeated.len(),
            across
        );
        for (era, (low, high, count)) in &census.era {
            println!("   {era:<10} {count:>8} commands, {low} .. {high}");
        }
    }

    if mode == "kinds" || mode == "flag" || mode == "check" || mode == "peractor" || mode == "states" {
        for (magic, per) in &census.kinds {
            let mut sorted: Vec<(&String, &usize)> = per.iter().collect();
            sorted.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
            let shown: Vec<String> = sorted
                .iter()
                .take(6)
                .map(|(kind, count)| format!("{kind} {count}"))
                .collect();
            println!("{magic:<6} {}", shown.join(", "));
            if let Some(names) = census.named.get(magic) {
                let mut sorted: Vec<(&String, &usize)> = names.iter().collect();
                sorted.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
                let shown: Vec<String> = sorted
                    .iter()
                    .take(8)
                    .map(|(name, count)| format!("{name} {count}"))
                    .collect();
                println!("       named {}", shown.join(", "));
            }
        }
    }

    println!(
        "{} files, {} unreadable, {} timelines",
        census.files, census.failed, census.timelines
    );
    println!(
        "scene actors reached by TMAC time {} by TMAC id {} by neither {}",
        census.reach[0], census.reach[1], census.reach[2]
    );

    if mode == "magics" {
        let mut wide: BTreeMap<Source, usize> = BTreeMap::new();
        let mut rolled: BTreeMap<String, BTreeMap<Source, usize>> = BTreeMap::new();
        for ((source, layout, magic), count) in &census.counts {
            if *layout {
                *wide.entry(*source).or_default() += count;
            }
            *rolled
                .entry(magic.clone())
                .or_default()
                .entry(*source)
                .or_default() += count;
        }
        println!("commands in wide-layout timelines: {wide:?}");
        println!("{:<8} {:>8} {:>8} {:>8} {:>8}", "magic", "sgb", "tmb", "pap", "cutb");
        for (magic, per) in &rolled {
            let of = |source: Source| per.get(&source).copied().unwrap_or(0);
            println!(
                "{magic:<8} {:>8} {:>8} {:>8} {:>8}",
                of(Source::Sgb),
                of(Source::Tmb),
                of(Source::Pap),
                of(Source::Cutb)
            );
        }
    }

    if mode == "fields" {
        for ((source, magic), shape) in &census.shapes {
            println!(
                "== {magic} in {source:?}: {} bodies in {} files, lengths {:?}, times {}..{}",
                shape.bodies,
                shape.files.len(),
                shape.lengths,
                shape.times.iter().next().copied().unwrap_or(0),
                shape.times.iter().next_back().copied().unwrap_or(0),
            );
            for (at, slot) in shape.slots.iter().enumerate() {
                let shown: Vec<String> = slot
                    .values
                    .iter()
                    .take(12)
                    .map(|(held, count)| format!("{held}x{count}"))
                    .collect();
                println!(
                    "   {at}: {} distinct  zero {} neg {} 1-64 {} curve {} actor {} actortime {} track {} cmd {} inst {} anim {} dur {} str {} float {}",
                    slot.values.len(),
                    slot.zero,
                    slot.negative,
                    slot.small,
                    slot.curve,
                    slot.actor,
                    slot.actor_time,
                    slot.track,
                    slot.command,
                    slot.instance,
                    slot.animated,
                    slot.duration,
                    slot.string,
                    slot.float,
                );
                let high: Vec<String> = slot
                    .values
                    .iter()
                    .rev()
                    .take(6)
                    .map(|(held, count)| format!("{held}x{count}"))
                    .collect();
                println!(
                    "      {}{}",
                    shown.join(" "),
                    match slot.values.len() > 12 {
                        true => format!(" ... {}", high.join(" ")),
                        false => String::new(),
                    }
                );
            }
        }
    }

    if mode == "tracks" {
        println!(
            "{} tracks hold {}, {} of them alone",
            census.tracks, census.wanted, census.alone
        );
        let mut sorted: Vec<(&String, &usize)> = census.together.iter().collect();
        sorted.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (magic, count) in sorted.iter().take(30) {
            println!("   with {magic}: {count}");
        }
    }
}
