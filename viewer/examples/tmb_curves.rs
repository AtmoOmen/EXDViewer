//! What the key record holds, which spans run straight, and how far the cubic reading moves a curve
//! away from the straight one. Reads the timelines of `.cutb` and `.sgb` alike.
//!
//! `tmb_curves <paths file>`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::sgb::SharedGroupFile;
use ironworks::file::tmb::{Curve, Curves, Item, Key, Timeline};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// How many points along a span the two readings are compared at.
const SAMPLES: usize = 16;

/// Reads a span the way a curve did before the cubic landed.
fn straight(from: &Key, to: &Key, time: f32) -> f32 {
    let span = to.time() - from.time();
    match span > 0.0 {
        true => from.value() + (to.value() - from.value()) * (time - from.time()) / span,
        false => from.value(),
    }
}

/// What a channel drives, for splitting the drift by the units it is measured in.
fn lane(channel: u8) -> &'static str {
    match channel {
        0..=2 => "shift",
        3..=5 => "turn",
        7..=9 => "size",
        48..=60 => "own",
        _ => "other",
    }
}

#[derive(Default)]
struct Spread {
    held: Vec<f32>,
    n: usize,
}

impl Spread {
    fn take(&mut self, value: f32) {
        if !value.is_finite() {
            return;
        }
        self.n += 1;
        if self.held.len() < 400_000 {
            self.held.push(value);
        }
    }

    fn report(&mut self, name: &str) {
        if self.held.is_empty() {
            println!("  {name}: nothing to report");
            return;
        }
        self.held.sort_by(f32::total_cmp);
        let at = |part: f64| self.held[((self.held.len() - 1) as f64 * part) as usize];
        println!(
            "  {name:<8} n {:>8}  median {:.4}  upper quartile {:.4}  99th {:.4}",
            self.n,
            at(0.5),
            at(0.75),
            at(0.99),
        );
    }
}

#[derive(Default)]
struct Census {
    read: usize,
    curves: usize,
    keys: usize,
    /// Keys by the flag at the head of the record.
    spans: [usize; 2],
    /// Keys whose two slopes are both zero, which a cubic still eases through.
    flat: usize,
    /// Whether the word at +0x08 is one over the span ahead, and zero at the last key.
    rate: [usize; 2],
    /// How far the cubic reading moves the curve, in the value's own units, per lane.
    drift: BTreeMap<&'static str, Spread>,
    /// Sets holding two curves that drive the same channel, and how many of those pairs stand for
    /// different targets.
    collisions: usize,
    across_targets: usize,
    sets: usize,
    /// How many targets a set names, from its own header.
    targets: BTreeMap<u32, usize>,
    /// Which target each tag ever names.
    tags: BTreeMap<u8, (usize, BTreeSet<u8>)>,
}

impl Census {
    fn curve(&mut self, curve: &Curve) {
        self.curves += 1;
        let keys = curve.keys();
        self.keys += keys.len();
        let held = self.tags.entry(curve.tag()).or_default();
        held.0 += 1;
        held.1.insert(curve.target());

        for (at, key) in keys.iter().enumerate() {
            self.spans[usize::from(key.linear())] += 1;
            self.flat += usize::from(key.slope_in() == 0.0 && key.slope_out() == 0.0);

            let stated = match keys.get(at + 1) {
                Some(after) => 1.0 / (after.time() - key.time()),
                None => 0.0,
            };
            self.rate[usize::from((key.rate() - stated).abs() <= 1e-6 * stated.abs())] += 1;
        }

        for span in keys.windows(2) {
            let (from, to) = (&span[0], &span[1]);
            if from.linear() {
                continue;
            }
            let widest = (1..SAMPLES)
                .map(|step| {
                    let time =
                        from.time() + (to.time() - from.time()) * step as f32 / SAMPLES as f32;
                    let held = curve.at(time).unwrap_or_default();
                    (held - straight(from, to, time)).abs()
                })
                .fold(0.0f32, f32::max);
            if widest > 0.0 {
                self.drift
                    .entry(lane(curve.tag() & 0x3F))
                    .or_default()
                    .take(widest);
            }
        }
    }

    fn set(&mut self, curves: &Curves) {
        self.sets += 1;
        *self.targets.entry(curves.targets()).or_default() += 1;

        let mut seen: BTreeMap<u8, u8> = BTreeMap::new();
        for curve in curves.curves() {
            let channel = curve.tag() & 0x3F;
            match seen.get(&channel) {
                Some(target) => {
                    self.collisions += 1;
                    self.across_targets += usize::from(*target != curve.target());
                }
                None => {
                    seen.insert(channel, curve.target());
                }
            }
        }
    }

    fn timeline(&mut self, timeline: &Timeline) {
        for item in timeline.items() {
            let Item::Curves(curves) = item else {
                continue;
            };
            self.set(curves);
            for curve in curves.curves() {
                self.curve(curve);
            }
        }
    }

    fn report(&mut self) {
        println!(
            "\n{} files, {} sets, {} curves, {} keys",
            self.read, self.sets, self.curves, self.keys
        );
        println!(
            "\n{} keys open a cubic span, {} open a straight one",
            self.spans[0], self.spans[1]
        );
        println!(
            "{} keys carry no slope either side, which a cubic still eases through",
            self.flat
        );
        println!(
            "\n+0x08 is one over the span ahead in {} keys and something else in {}",
            self.rate[1], self.rate[0]
        );

        println!("\nhow far a cubic span moves from the straight reading, in the value's own units");
        for (lane, spread) in &mut self.drift {
            spread.report(lane);
        }

        println!("\ntargets a set names");
        for (targets, count) in &self.targets {
            println!("  {targets:>3}: {count}");
        }
        println!(
            "\n{} pairs of curves in one set drive the same channel, {} of them across different \
             targets",
            self.collisions, self.across_targets
        );

        println!("\nthe targets each tag ever names");
        for (tag, (count, targets)) in &self.tags {
            let held: Vec<String> = targets.iter().map(u8::to_string).collect();
            println!(
                "  {tag:#04x} channel {:>2}: {count:>7} curves over targets {}",
                tag & 0x3F,
                held.join(",")
            );
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a paths file");
    let paths = std::fs::read_to_string(list).expect("the paths file");

    let mut census = Census::default();
    for path in paths.lines() {
        if path.ends_with(".cutb") {
            let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
                continue;
            };
            let Ok(file) = Cutscene::read(std::io::Cursor::new(bytes)) else {
                continue;
            };
            census.read += 1;
            for node in file.nodes() {
                if let Node::Timeline(timeline) = node {
                    census.timeline(timeline);
                }
            }
        } else if path.ends_with(".sgb") {
            let Ok(file) = ironworks.file::<SharedGroupFile>(path) else {
                continue;
            };
            census.read += 1;
            for held in file.scene().timelines() {
                census.timeline(held.timeline());
            }
        }
    }
    census.report();
}
