//! Where a cutscene's cameras stand over each shot, and how far the cubic reading moves them away
//! from the straight one.
//!
//! `cutb_shots <cutb path>`

use std::collections::BTreeSet;

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::tmb::{Channel, CommandKind, Curve, Curves, Item};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// How many samples a frame of the shot is read at.
const OVER: usize = 4;

/// The camera's own field of view, which sits past the transform channels.
const FIELD: u8 = 52;

/// Reads a curve the way it read before the cubic landed.
fn straight(curve: &Curve, time: f32) -> Option<f32> {
    let keys = curve.keys();
    let last = keys.last()?;
    if time >= last.time() {
        return Some(last.value());
    }
    let first = keys.first()?;
    if time <= first.time() {
        return Some(first.value());
    }
    let at = keys.windows(2).find(|pair| time < pair[1].time())?;
    let span = at[1].time() - at[0].time();
    Some(match span > 0.0 {
        true => at[0].value() + (at[1].value() - at[0].value()) * (time - at[0].time()) / span,
        false => at[0].value(),
    })
}

fn place(set: &Curves, target: u8, time: f32, cubic: bool) -> [f32; 3] {
    [Channel::TranslationX, Channel::TranslationY, Channel::TranslationZ]
        .map(|channel| match set.channel(target, channel) {
            Some(curve) => match cubic {
                true => curve.at(time),
                false => straight(curve, time),
            }
            .unwrap_or_default(),
            None => 0.0,
        })
}

fn field(set: &Curves, time: f32) -> Option<(f32, f32)> {
    let curve = set
        .curves()
        .iter()
        .find(|curve| curve.tag() & 0x3F == FIELD)?;
    Some((curve.at(time)?, straight(curve, time)?))
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let path = std::env::args().nth(1).expect("a cutb path");
    let bytes = ironworks.file::<Vec<u8>>(&path).expect("the cutscene");
    let file = Cutscene::read(std::io::Cursor::new(bytes)).expect("a cutscene");

    println!("{path}");
    for node in file.nodes() {
        let Node::Timeline(timeline) = node else {
            continue;
        };
        for item in timeline.items() {
            let Item::Command(command) = item else {
                continue;
            };
            let CommandKind::C004(camera) = command.kind() else {
                continue;
            };
            let Some(set) = timeline.items().iter().find_map(|item| match item {
                Item::Curves(held) if i32::from(held.id()) == camera.curve_id() => Some(held),
                _ => None,
            }) else {
                continue;
            };

            let targets: BTreeSet<u8> = set
                .curves()
                .iter()
                .filter(|curve| curve.channel() == Some(Channel::TranslationX))
                .map(Curve::target)
                .collect();
            let frames = camera.duration().max(1) as usize;
            let mut widest = 0.0f32;
            let mut total = 0.0f64;
            let mut lens = 0.0f32;
            for step in 0..=frames * OVER {
                let time = step as f32 / OVER as f32;
                let mut apart = 0.0f32;
                for target in &targets {
                    let (curved, flat) = (
                        place(set, *target, time, true),
                        place(set, *target, time, false),
                    );
                    apart = apart.max(
                        (0..3)
                            .map(|axis| (curved[axis] - flat[axis]).powi(2))
                            .sum::<f32>()
                            .sqrt(),
                    );
                }
                widest = widest.max(apart);
                total += f64::from(apart);
                if let Some((curved, flat)) = field(set, time) {
                    lens = lens.max((curved - flat).abs());
                }
            }

            println!(
                "  {:<12} {frames:>4} frames, set {:>3} over {} targets, {} curves",
                camera.name().unwrap_or("-"),
                set.id(),
                set.targets(),
                set.curves().len(),
            );
            println!(
                "     the cubic camera stands up to {widest:.3} units from the straight one, \
                 {:.3} on average, and its lens differs by up to {lens:.3} degrees",
                total / (frames * OVER + 1) as f64,
            );
        }
    }
}
