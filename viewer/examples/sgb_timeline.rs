//! What a shared group animates, out of the timeline region its scene header points back at.

use ironworks::file::{sgb::SharedGroupFile, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const SGB: &str = "bgcommon/world/aet/shared/for_bg/sgbg_w_aet_001_01a.sgb";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args()
        .skip(1)
        .chain(std::iter::once(SGB.to_owned()))
        .take(8)
    {
        let file: SharedGroupFile = match ironworks.file(&path) {
            Ok(held) => held,
            Err(why) => {
                println!("== {path}: {why}");
                continue;
            }
        };
        let held = file.scene();
        println!("== {path}  {} timelines", held.timelines().len());
        for group in held.layer_groups() {
            for layer in group.layers() {
                for instance in layer.instances() {
                    if let ironworks::file::layer::InstanceData::SharedGroup(child) =
                        instance.data()
                    {
                        println!("   child #{:<4} {}", instance.id(), child.asset_path());
                    }
                }
            }
        }
        for timeline in held.timelines() {
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
                        println!("      TMFC id {:>3}", curves.id());
                        for curve in curves.curves() {
                            let keys: Vec<String> = curve
                                .keys()
                                .iter()
                                .map(|key| format!("{:.0}->{:.3}", key.time(), key.value()))
                                .collect();
                            println!(
                                "         {:?}  {} keys  {}",
                                curve.channel(),
                                curve.keys().len(),
                                keys.join("  ")
                            );
                        }
                    }
                    tmb::Item::Track(track) => println!(
                        "      TMTR id {:>3}  commands {:?}",
                        track.id(),
                        track.commands()
                    ),
                    tmb::Item::Command(command) => {
                        println!("      TMAL id {:>3}  {:?}", command.id(), command.kind())
                    }
                    tmb::Item::Actor(actor) => println!(
                        "      TMAC id {:>3} time {} delay {} unk {}  tracks {:?}",
                        actor.id(),
                        actor.time(),
                        actor.ability_delay(),
                        actor.unknown_2(),
                        actor.tracks()
                    ),
                    tmb::Item::Header(header) => println!(
                        "      TMDH id {} duration {}",
                        header.id(),
                        header.duration()
                    ),
                    held => {
                        if let Some(id) = held.id() {
                            println!("      {:?} id {id}", std::mem::discriminant(held));
                        }
                    }
                }
            }
        }
    }
}
