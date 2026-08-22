//! Every transform a scene's timelines state for one of its own nodes at nought, auto-playing or
//! not, against where the scene itself placed that node.
//!
//! `sgb_states <sgb paths file> [scene path]`

use std::collections::BTreeMap;

use ironworks::file::{sgb::SharedGroupFile, tmb};
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
    let list = std::fs::read_to_string(args.next().expect("an sgb paths file")).unwrap();
    let only = args.next().unwrap_or_default();

    let mut alone = 0usize;
    let mut sibling_seats = 0usize;
    let mut sibling_elsewhere = 0usize;
    let mut auto_seats = 0usize;
    let mut siblings: BTreeMap<usize, usize> = BTreeMap::new();
    let mut loops: BTreeMap<(bool, bool), usize> = BTreeMap::new();

    for path in list.lines() {
        if !only.is_empty() && path != only {
            continue;
        }
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
        // Per node, every (timeline name, auto-play, looping, transform stated at nought).
        let mut stated: BTreeMap<i32, Vec<(String, bool, bool, [f32; 3])>> = BTreeMap::new();
        for timeline in scene.timelines() {
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
                            stated.entry(*instance).or_default().push((
                                timeline.kind().clone(),
                                timeline.auto_play(),
                                timeline.looping(),
                                driven.translation(),
                            ));
                        }
                    }
                }
            }
        }
        for (instance, held) in &stated {
            let Some(seat) = seats.get(instance) else {
                continue;
            };
            let Some(auto) = held.iter().find(|(_, auto, _, _)| *auto) else {
                continue;
            };
            if reach(auto.3, *seat) < 10.0 {
                continue;
            }
            *loops.entry((auto.2, held.len() > 1)).or_default() += 1;
            let rest: Vec<&(String, bool, bool, [f32; 3])> =
                held.iter().filter(|(_, auto, _, _)| !*auto).collect();
            *siblings.entry(rest.len()).or_default() += 1;
            if rest.is_empty() {
                alone += 1;
            } else if rest.iter().any(|(_, _, _, at)| reach(*at, *seat) < 0.001) {
                sibling_seats += 1;
            } else {
                sibling_elsewhere += 1;
            }
            if reach(auto.3, *seat) < 0.001 {
                auto_seats += 1;
            }
            if !only.is_empty() {
                println!("#{instance} placed [{:.1} {:.1} {:.1}]", seat[0], seat[1], seat[2]);
                for (name, auto, looping, at) in held {
                    println!(
                        "   {name:<24} auto {auto:<5} loop {looping:<5} [{:.1} {:.1} {:.1}]{}",
                        at[0],
                        at[1],
                        at[2],
                        match reach(*at, *seat) < 0.001 {
                            true => "  = placement",
                            false => "",
                        }
                    );
                }
            }
        }
    }
    println!("relocating nodes whose auto-playing timeline is the only one to state a transform: {alone}");
    println!("with a sibling timeline stating the placement: {sibling_seats}");
    println!("with siblings, none stating the placement: {sibling_elsewhere}");
    println!("auto-playing transform equal to the placement: {auto_seats}");
    println!("sibling count: {siblings:?}");
    println!("(looping, more than one timeline): {loops:?}");
}
