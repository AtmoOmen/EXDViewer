//! Every instance an autoplaying timeline states a transform for with no curve to play it over:
//! what the file placed it at, what the timeline states, and how far apart the two are.
//!
//! `sgb_placed <paths file | path> [more paths]`

use std::collections::BTreeMap;

use ironworks::file::layer::{Instance, InstanceKind, SceneTimeline};
use ironworks::file::{sgb::SharedGroupFile, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

struct Step {
    time: f32,
    duration: i32,
    translation: [f32; 3],
    rotation: [f32; 3],
}

fn reach(left: [f32; 3], right: [f32; 3]) -> f32 {
    let held = [
        left[0] - right[0],
        left[1] - right[1],
        left[2] - right[2],
    ];
    (held[0] * held[0] + held[1] * held[1] + held[2] * held[2]).sqrt()
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<String> = match args.first().map(std::fs::read_to_string) {
        Some(Ok(list)) => list.lines().map(str::to_owned).collect(),
        _ => args,
    };

    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut looped: BTreeMap<(String, bool, bool), usize> = BTreeMap::new();
    let mut placed = 0usize;
    let mut keyed = 0usize;
    let mut at_origin = 0usize;
    let mut first_at_zero = 0usize;
    let mut first_restates = 0usize;
    let mut nothing = 0usize;
    let mut tiny = 0usize;
    let mut far: Vec<String> = Vec::new();
    let mut far_kinds: BTreeMap<(String, bool, bool), usize> = BTreeMap::new();
    let mut far_instances: BTreeMap<String, usize> = BTreeMap::new();
    let mut durations: BTreeMap<i32, usize> = BTreeMap::new();
    let mut far_durations: BTreeMap<i32, usize> = BTreeMap::new();
    let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut turned = 0usize;
    let mut keyed_loops: BTreeMap<bool, usize> = BTreeMap::new();
    let mut driving: BTreeMap<(String, i32), usize> = BTreeMap::new();
    let mut aet_loops: BTreeMap<bool, usize> = BTreeMap::new();

    for path in &paths {
        let Ok(file) = ironworks.file::<SharedGroupFile>(path.as_str()) else {
            continue;
        };
        let scene = file.scene();
        let held: Vec<&Instance> = scene
            .layer_groups()
            .iter()
            .flat_map(|group| group.layers())
            .flat_map(|layer| layer.instances())
            .collect();
        for timeline in scene.timelines() {
            *looped
                .entry((
                    timeline.kind().clone(),
                    timeline.auto_play(),
                    timeline.looping(),
                ))
                .or_default() += 1;
            *kinds.entry(timeline.kind().clone()).or_default() += 1;
            if !timeline.auto_play() {
                continue;
            }
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
                let mut curves = 0usize;
                let mut steps: Vec<Step> = Vec::new();
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
                        match found.kind() {
                            tmb::CommandKind::C013(_) => curves += 1,
                            tmb::CommandKind::C018(driven) => steps.push(Step {
                                time: f32::from(found.time()),
                                duration: driven.duration(),
                                translation: driven.translation(),
                                rotation: driven.rotation(),
                            }),
                            _ => (),
                        }
                    }
                }
                if curves > 0 {
                    *driving.entry((path.clone(), *instance)).or_default() += 1;
                    keyed += 1;
                    *keyed_loops.entry(timeline.looping()).or_default() += 1;
                    if path.contains("/aet/") {
                        *aet_loops.entry(timeline.looping()).or_default() += 1;
                    }
                    continue;
                }
                if steps.is_empty() {
                    continue;
                }
                steps.sort_by(|left, right| left.time.total_cmp(&right.time));
                placed += 1;
                *counts.entry(steps.len()).or_default() += 1;
                let Some(found) = held.iter().find(|item| item.id() as i32 == *instance) else {
                    continue;
                };
                let seat = found.transform().translation();
                let rest = found.transform().rotation();
                if seat == [0.0, 0.0, 0.0] {
                    at_origin += 1;
                }
                if steps[0].time == 0.0 {
                    first_at_zero += 1;
                }
                if reach(steps[0].translation, seat) < 0.001 {
                    first_restates += 1;
                }
                for step in &steps {
                    *durations.entry(step.duration).or_default() += 1;
                }
                let worst = steps
                    .iter()
                    .map(|step| reach(step.translation, seat))
                    .fold(0.0f32, f32::max);
                if worst < 1e-6 {
                    nothing += 1;
                } else if worst < 0.001 {
                    tiny += 1;
                }
                if worst >= 10.0 {
                    *far_kinds
                        .entry((
                            timeline.kind().clone(),
                            timeline.auto_play(),
                            timeline.looping(),
                        ))
                        .or_default() += 1;
                    *far_instances
                        .entry(format!("{:?}", found.kind()))
                        .or_default() += 1;
                    for step in &steps {
                        *far_durations.entry(step.duration).or_default() += 1;
                    }
                    let spun = steps
                        .iter()
                        .any(|step| reach(step.rotation, rest) > 0.001);
                    turned += usize::from(spun);
                    far.push(format!(
                        "{path} #{instance} {:?} {:?} loop {} at [{:.1} {:.1} {:.1}] steps {} worst {worst:.1} spun {spun} | {}",
                        found.kind(),
                        timeline.kind(),
                        timeline.looping(),
                        seat[0],
                        seat[1],
                        seat[2],
                        steps.len(),
                        steps
                            .iter()
                            .map(|step| format!(
                                "t{:.0}/d{} [{:.1} {:.1} {:.1}]",
                                step.time, step.duration, step.translation[0], step.translation[1], step.translation[2]
                            ))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
            }
        }
    }

    println!("timeline end kinds: {kinds:?}");
    println!("(kind, auto, loop): {looped:?}");
    println!(
        "placed {placed}  keyed {keyed}  at origin {at_origin}  first step at t=0 {first_at_zero}  first step restates placement {first_restates}"
    );
    println!("exact no-op {nothing}  under a millimetre {tiny}  ten metres or more {}", far.len());
    println!("steps per instance: {counts:?}");
    println!("C018 duration, all placed: {durations:?}");
    println!("C018 duration, ten metres or more: {far_durations:?}");
    println!("ten metres or more, by (kind, auto, loop): {far_kinds:?}");
    println!("ten metres or more, by instance kind: {far_instances:?}");
    println!("ten metres or more, also turning: {turned}");
    println!("auto-playing curve actors by loop flag: {keyed_loops:?}  under /aet/: {aet_loops:?}");
    let mut contended: BTreeMap<usize, usize> = BTreeMap::new();
    for count in driving.values() {
        *contended.entry(*count).or_default() += 1;
    }
    println!("nodes by the number of auto-playing timelines whose curves reach them: {contended:?}");
    let _ = InstanceKind::None;
    let _ = SceneTimeline::of;
    for line in &far {
        println!("   {line}");
    }
}
