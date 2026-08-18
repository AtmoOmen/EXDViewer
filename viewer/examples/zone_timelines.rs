//! Every shared group a zone places, and what each of them animates.
//!
//! `zone_timelines <path.lvb> [name filter]`

use std::collections::BTreeSet;

use ironworks::file::layer::InstanceData;
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;

fn groups(ironworks: &Ironworks<SqPack<Install>>, path: &str, into: &mut BTreeSet<String>, depth: usize) {
    if depth > DEPTH || !into.insert(path.to_owned()) {
        return;
    }
    let Ok(file) = ironworks.file::<SharedGroupFile>(path) else {
        return;
    };
    let mut children = Vec::new();
    for group in file.scene().layer_groups() {
        for layer in group.layers() {
            for instance in layer.instances() {
                if let InstanceData::SharedGroup(held) = instance.data()
                    && !held.asset_path().is_empty()
                {
                    children.push(held.asset_path().clone());
                }
            }
        }
    }
    for child in children {
        groups(ironworks, &child, into, depth + 1);
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let zone = args.next().expect("a level path");
    let filter = args.next().unwrap_or_default();

    let level: LevelFile = ironworks.file(&zone).unwrap();
    let mut found = BTreeSet::new();
    for path in level.scene().layer_group_paths() {
        let Ok(file) = ironworks.file::<LayerGroupFile>(path) else {
            println!("== {path}: unreadable");
            continue;
        };
        for layer in file.group().layers() {
            for instance in layer.instances() {
                if let InstanceData::SharedGroup(held) = instance.data()
                    && !held.asset_path().is_empty()
                {
                    groups(&ironworks, held.asset_path(), &mut found, 0);
                }
            }
        }
    }
    println!("== {zone}  {} shared groups", found.len());

    for path in found.iter().filter(|path| path.contains(&filter)) {
        let Ok(file) = ironworks.file::<SharedGroupFile>(path) else {
            continue;
        };
        let scene = file.scene();
        let mut drawn = Vec::new();
        let mut nested = Vec::new();
        for group in scene.layer_groups() {
            for layer in group.layers() {
                for instance in layer.instances() {
                    match instance.data() {
                        InstanceData::BgPart(held) if !held.asset_path().is_empty() => {
                            drawn.push((instance.id(), held.asset_path().clone()));
                        }
                        InstanceData::SharedGroup(held) if !held.asset_path().is_empty() => {
                            nested.push((instance.id(), held.asset_path().clone()));
                        }
                        _ => (),
                    }
                }
            }
        }
        if scene.timelines().is_empty() && filter.is_empty() {
            continue;
        }
        println!(
            "\n== {path}  {} timelines, {} parts, {} groups",
            scene.timelines().len(),
            drawn.len(),
            nested.len()
        );
        for (id, held) in drawn.iter().chain(&nested) {
            println!("   #{id:<5} {held}");
        }
        for timeline in scene.timelines() {
            println!(
                "   sub {} kind {:?} auto {} loop {}  drives {:?}",
                timeline.sub_id(),
                timeline.kind(),
                timeline.auto_play(),
                timeline.looping(),
                timeline.animated(),
            );
            for item in timeline.timeline().items() {
                match item {
                    tmb::Item::Curves(curves) => {
                        for curve in curves.curves() {
                            let keys: Vec<String> = curve
                                .keys()
                                .iter()
                                .map(|key| format!("{:.0}->{:.2}", key.time(), key.value()))
                                .collect();
                            if curve.keys().len() > 1 {
                                println!(
                                    "      TMFC {:>3}  {:?}  {}",
                                    curves.id(),
                                    curve.channel(),
                                    keys.join("  ")
                                );
                            }
                        }
                    }
                    tmb::Item::Actor(actor) => println!(
                        "      TMAC id {:>3} time {}  tracks {:?}",
                        actor.id(),
                        actor.time(),
                        actor.tracks()
                    ),
                    tmb::Item::Track(track) => println!(
                        "      TMTR id {:>3}  commands {:?}",
                        track.id(),
                        track.commands()
                    ),
                    tmb::Item::Command(command) => {
                        println!("      TMAL id {:>3}  {:?}", command.id(), command.kind())
                    }
                    _ => (),
                }
            }
        }
    }
}
