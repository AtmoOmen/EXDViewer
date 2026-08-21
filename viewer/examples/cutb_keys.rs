//! Every key of the longer curves in one cutscene, beside the slopes its neighbours imply.
//!
//! `cutb_keys <cutb path, or a paths file to scan> [least keys]`

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::tmb::{Curve, Item};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// How many curves a scan over the corpus stops after.
const SHOWN: usize = 8;

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let held = std::env::args().nth(1).expect("a cutb path or a paths file");
    let least: usize = std::env::args()
        .nth(2)
        .and_then(|held| held.parse().ok())
        .unwrap_or(8);

    let paths: Vec<String> = match held.ends_with(".cutb") {
        true => vec![held],
        false => std::fs::read_to_string(held)
            .expect("the paths file")
            .lines()
            .map(str::to_owned)
            .collect(),
    };

    let scanning = paths.len() > 1;
    let mut shown = 0;
    for path in &paths {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        for node in file.nodes() {
            let Node::Timeline(timeline) = node else {
                continue;
            };
            for item in timeline.items() {
                let Item::Curves(curves) = item else {
                    continue;
                };
                for curve in curves.curves() {
                    if curve.keys().len() < least {
                        continue;
                    }
                    if scanning && !moving(curve) {
                        continue;
                    }
                    show(path, curves.id(), curve);
                    shown += 1;
                    if scanning && shown >= SHOWN {
                        return;
                    }
                }
            }
        }
    }
}

/// Whether the curve covers enough ground to read anything off it.
fn moving(curve: &Curve) -> bool {
    let (low, high) = curve
        .keys()
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |held, key| {
            (held.0.min(key.value()), held.1.max(key.value()))
        });
    high - low > 1.0
}

fn show(path: &str, set: i16, curve: &Curve) {
    let keys = curve.keys();
    println!(
        "\n{path} set {set} tag {:#04x} channel {} target {}, {} keys",
        curve.tag(),
        curve.tag() & 0x3F,
        curve.target(),
        keys.len()
    );
    println!(
        "   {:>6} {:>8} {:>12} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14}",
        "linear", "time", "rate", "value", "in", "out", "central", "behind", "ahead"
    );
    for (at, key) in keys.iter().enumerate() {
        let behind = keys
            .get(at.wrapping_sub(1))
            .map(|before| (key.value() - before.value()) / (key.time() - before.time()));
        let ahead = keys
            .get(at + 1)
            .map(|after| (after.value() - key.value()) / (after.time() - key.time()));
        let central = match (keys.get(at.wrapping_sub(1)), keys.get(at + 1)) {
            (Some(before), Some(after)) => {
                Some((after.value() - before.value()) / (after.time() - before.time()))
            }
            _ => None,
        };
        let show = |held: Option<f32>| match held {
            Some(held) => format!("{held:>14.6}"),
            None => format!("{:>14}", "-"),
        };
        println!(
            "   {:>6} {:>8.2} {:>12.6} {:>14.6} {:>14.6} {:>14.6} {} {} {}",
            key.linear(),
            key.time(),
            key.rate(),
            key.value(),
            key.slope_in(),
            key.slope_out(),
            show(central),
            show(behind),
            show(ahead),
        );
    }
}
