//! What a timeline's time unit is worth, out of the one place a file states both: an animation
//! pack holds a Havok motion in seconds beside the timeline that drives it in ticks.
//!
//! `pap_ticks [stride]`

use std::collections::BTreeMap;

use ironworks::file::{File, pap::AnimationPack, tmb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

fn bucket(rate: f32) -> String {
    format!("{:.1}", (rate * 4.0).round() / 4.0)
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let stride: usize = std::env::args()
        .nth(1)
        .and_then(|held| held.parse().ok())
        .unwrap_or(40);

    let listed = std::fs::read_to_string(PATHS).expect("the path list");
    let paths: Vec<&str> = listed
        .lines()
        .filter(|path| path.ends_with(".pap"))
        .step_by(stride)
        .collect();

    let mut authored: BTreeMap<String, usize> = BTreeMap::new();
    let mut against: BTreeMap<String, usize> = BTreeMap::new();
    let mut equal = 0usize;
    let mut examples = Vec::new();
    let mut packs = 0usize;
    let mut pairs = 0usize;

    for path in &paths {
        let Ok(pack) = ironworks.file::<AnimationPack>(*path) else {
            continue;
        };
        let Ok(bindings) = pack.parse_animations() else {
            continue;
        };
        packs += 1;
        for (index, animation) in pack.animations().iter().enumerate() {
            let Some(binding) = usize::try_from(animation.havok_index())
                .ok()
                .and_then(|at| bindings.get(at))
            else {
                continue;
            };
            let motion = binding.motion();
            let seconds = motion.duration();
            if seconds <= 0.0 || motion.frames() < 2 {
                continue;
            }
            *authored
                .entry(bucket((motion.frames() - 1) as f32 / seconds))
                .or_default() += 1;

            let Some(bytes) = pack.timelines().get(index) else {
                continue;
            };
            let Ok(timeline) = tmb::Timeline::read(std::io::Cursor::new(bytes.clone())) else {
                continue;
            };
            let Some(ticks) = timeline.items().iter().find_map(|item| match item {
                tmb::Item::Header(held) => Some(f32::from(held.duration())),
                _ => None,
            }) else {
                continue;
            };
            if ticks <= 0.0 {
                continue;
            }
            pairs += 1;
            equal += usize::from((ticks - (motion.frames() - 1) as f32).abs() < 0.5);
            *against.entry(bucket(ticks / seconds)).or_default() += 1;
            if examples.len() < 20 {
                examples.push(format!(
                    "{path} #{index} {} ticks over {seconds:.4}s in {} frames",
                    ticks,
                    motion.frames()
                ));
            }
        }
    }

    println!("{packs} packs of {} sampled, {pairs} pairs", paths.len());
    println!("authored frame rate, (frames - 1) / duration:");
    for (rate, count) in &authored {
        println!("   {rate:>8}  {count}");
    }
    println!("timeline duration over animation seconds:");
    for (rate, count) in &against {
        println!("   {rate:>8}  {count}");
    }
    println!("timeline duration equals the animation's last frame in {equal} of {pairs}");
    for line in &examples {
        println!("   {line}");
    }
}
