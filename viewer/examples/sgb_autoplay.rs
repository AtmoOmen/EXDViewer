//! Whether a scene's timelines are played at load: the scene's own auto-play byte against the
//! `random_timeline_auto_play` the instance that places the scene carries, and whether a scene ever
//! states two auto-playing transforms for one node at once.
//!
//! `sgb_autoplay <sgb paths file> <lgb paths file>`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::layer::InstanceData;
use ironworks::file::{lgb::LayerGroupFile, sgb::SharedGroupFile, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn reach(left: [f32; 3], right: [f32; 3]) -> f32 {
    let held = [left[0] - right[0], left[1] - right[1], left[2] - right[2]];
    (held[0] * held[0] + held[1] * held[1] + held[2] * held[2]).sqrt()
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let scenes = std::fs::read_to_string(args.next().expect("an sgb paths file")).unwrap();
    let groups = std::fs::read_to_string(args.next().expect("an lgb paths file")).unwrap();

    let mut far: BTreeSet<String> = BTreeSet::new();
    let mut steady: BTreeSet<String> = BTreeSet::new();
    let mut timelines: BTreeMap<bool, usize> = BTreeMap::new();
    let mut per_scene: BTreeMap<usize, usize> = BTreeMap::new();
    let mut contended = 0usize;
    let mut agreeing = 0usize;
    let mut examples: Vec<String> = Vec::new();

    for path in scenes.lines() {
        let Ok(file) = ironworks.file::<SharedGroupFile>(path) else {
            continue;
        };
        let scene = file.scene();
        let seats: BTreeMap<i32, [f32; 3]> = scene
            .layer_groups()
            .iter()
            .flat_map(|group| group.layers())
            .flat_map(|layer| layer.instances())
            .map(|instance| (instance.id() as i32, instance.transform().translation()))
            .collect();
        let mut auto = 0usize;
        // Every transform an auto-playing timeline states at nought for one node of this scene.
        let mut stated: BTreeMap<i32, Vec<(String, [f32; 3])>> = BTreeMap::new();
        for timeline in scene.timelines() {
            *timelines.entry(timeline.auto_play()).or_default() += 1;
            if !timeline.auto_play() {
                continue;
            }
            auto += 1;
            let items = timeline.timeline().items();
            for (actor, instance) in timeline.animated() {
                let Some(tracks) = items.iter().find_map(|item| match item {
                    tmb::Item::Actor(held) if i32::from(held.time()) == *actor => {
                        Some(held.tracks())
                    }
                    _ => None,
                }) else {
                    continue;
                };
                for track in tracks {
                    let Some(commands) = items.iter().find_map(|item| match item {
                        tmb::Item::Track(held) if held.id() == *track => Some(held.commands()),
                        _ => None,
                    }) else {
                        continue;
                    };
                    for command in commands {
                        let Some(tmb::Item::Command(found)) = items.iter().find(|item| {
                            matches!(item, tmb::Item::Command(held) if held.id() == *command)
                        }) else {
                            continue;
                        };
                        if let tmb::CommandKind::C018(driven) = found.kind()
                            && found.time() == 0
                        {
                            stated
                                .entry(*instance)
                                .or_default()
                                .push((timeline.kind().clone(), driven.translation()));
                        }
                    }
                }
            }
        }
        *per_scene.entry(auto).or_default() += 1;
        for (instance, held) in &stated {
            if held.len() < 2 {
                continue;
            }
            let split = held
                .iter()
                .any(|(_, at)| reach(*at, held[0].1) > 0.001);
            contended += usize::from(split);
            agreeing += usize::from(!split);
            if split && examples.len() < 12 {
                examples.push(format!(
                    "{path} #{instance} {}",
                    held.iter()
                        .map(|(name, at)| format!("{name} [{:.1} {:.1} {:.1}]", at[0], at[1], at[2]))
                        .collect::<Vec<_>>()
                        .join("  ")
                ));
            }
            if let Some(seat) = seats.get(instance)
                && held.iter().any(|(_, at)| reach(*at, *seat) >= 10.0)
                && split
            {
                far.insert(path.to_owned());
            }
        }
        for (instance, held) in &stated {
            let Some(seat) = seats.get(instance) else {
                continue;
            };
            if held.iter().any(|(_, at)| reach(*at, *seat) >= 10.0) {
                far.insert(path.to_owned());
            } else {
                steady.insert(path.to_owned());
            }
        }
    }
    println!(
        "scene timelines by their own auto-play byte: {timelines:?}\nauto-playing timelines per scene: {per_scene:?}"
    );
    println!(
        "one node, two auto-playing transforms: {contended} disagree, {agreeing} agree\n{} scenes relocating a node ten metres or more",
        far.len()
    );
    for line in &examples {
        println!("   {line}");
    }

    // What the instances that place those scenes say about playing their timelines.
    let mut placing: BTreeMap<(bool, bool, bool), usize> = BTreeMap::new();
    let mut every: BTreeMap<(bool, bool), usize> = BTreeMap::new();
    let mut walked = 0usize;
    let mut visit = |file: &dyn Fn() -> Option<Vec<(String, bool, bool)>>| {
        if let Some(found) = file() {
            walked += 1;
            for (asset, auto, looping) in found {
                *every.entry((auto, looping)).or_default() += 1;
                if far.contains(&asset) {
                    *placing.entry((true, auto, looping)).or_default() += 1;
                } else if steady.contains(&asset) {
                    *placing.entry((false, auto, looping)).or_default() += 1;
                }
            }
        }
    };
    for path in groups.lines().chain(scenes.lines()) {
        visit(&|| {
            let held: Vec<(String, bool, bool)> = if path.ends_with(".lgb") {
                let file = ironworks.file::<LayerGroupFile>(path).ok()?;
                file.group()
                    .layers()
                    .iter()
                    .flat_map(|layer| layer.instances())
                    .filter_map(|instance| match instance.data() {
                        InstanceData::SharedGroup(held) => Some((
                            held.asset_path().clone(),
                            held.random_timeline_auto_play(),
                            held.random_timeline_loop_playback(),
                        )),
                        _ => None,
                    })
                    .collect()
            } else {
                let file = ironworks.file::<SharedGroupFile>(path).ok()?;
                file.scene()
                    .layer_groups()
                    .iter()
                    .flat_map(|group| group.layers())
                    .flat_map(|layer| layer.instances())
                    .filter_map(|instance| match instance.data() {
                        InstanceData::SharedGroup(held) => Some((
                            held.asset_path().clone(),
                            held.random_timeline_auto_play(),
                            held.random_timeline_loop_playback(),
                        )),
                        _ => None,
                    })
                    .collect()
            };
            Some(held)
        });
    }
    println!("{walked} files walked for the instances that place a scene");
    println!("every shared group instance by (auto, loop): {every:?}");
    println!("(relocates, auto, loop): {placing:?}");
}
