//! What a zone's `Vfx` layer instances actually declare: colour, fade gates, auto play, and
//! whether the fade_near/fade_far pairs order the way a distance fade would need.
//!
//! `zone_vfx_census`

use std::collections::BTreeMap;

use ironworks::file::layer::{InstanceData, LayerGroup};
use ironworks::file::lvb;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

#[derive(Default)]
struct Tally {
    total: usize,
    auto_play: usize,
    no_far_clip: usize,
    non_white: usize,
    alpha_zero: usize,
    rgba_counts: BTreeMap<(u8, u8, u8, u8), usize>,
    near_ordered: usize,
    far_ordered: usize,
    far_ordered_clip: usize,
    far_ordered_no_clip: usize,
    clip_total: usize,
    no_clip_total: usize,
    near_before_far: usize,
    near_touches_far: usize,
    soft_particle_zero: usize,
    soft_particle_values: BTreeMap<String, usize>,
    z_correct_zero: usize,
    z_correct_values: BTreeMap<String, usize>,
    examples_non_white: Vec<String>,
    examples_bad_order: Vec<String>,
}

fn visit(group: &LayerGroup, tally: &mut Tally, path: &str) {
    for layer in group.layers() {
        for instance in layer.instances() {
            let InstanceData::Vfx(vfx) = instance.data() else {
                continue;
            };
            if vfx.asset_path().is_empty() {
                continue;
            }
            tally.total += 1;
            if vfx.auto_play() {
                tally.auto_play += 1;
            }
            if vfx.no_far_clip() {
                tally.no_far_clip += 1;
            }
            let colour = vfx.colour();
            let rgba = (colour.red(), colour.green(), colour.blue(), colour.alpha());
            *tally.rgba_counts.entry(rgba).or_default() += 1;
            if rgba != (255, 255, 255, 255) {
                tally.non_white += 1;
                if rgba.3 == 0 {
                    tally.alpha_zero += 1;
                }
                if tally.examples_non_white.len() < 10 {
                    tally.examples_non_white.push(format!(
                        "{path} #{} {:?} rgba={rgba:?}",
                        instance.id(),
                        vfx.asset_path()
                    ));
                }
            }
            let [n0, n1] = vfx.fade_near();
            let [f0, f1] = vfx.fade_far();
            if n0 <= n1 {
                tally.near_ordered += 1;
            }
            if f0 <= f1 {
                tally.far_ordered += 1;
            }
            match vfx.no_far_clip() {
                true => {
                    tally.no_clip_total += 1;
                    if f0 <= f1 {
                        tally.far_ordered_no_clip += 1;
                    }
                }
                false => {
                    tally.clip_total += 1;
                    if f0 <= f1 {
                        tally.far_ordered_clip += 1;
                    }
                }
            }
            if n1 <= f0 {
                tally.near_before_far += 1;
            } else if tally.examples_bad_order.len() < 10 {
                tally.examples_bad_order.push(format!(
                    "{path} #{} near={n0:?}..{n1:?} far={f0:?}..{f1:?}",
                    instance.id()
                ));
            }
            if n1 == f0 {
                tally.near_touches_far += 1;
            }
            let range = vfx.soft_particle_fade_range();
            if range == 0.0 {
                tally.soft_particle_zero += 1;
            }
            *tally
                .soft_particle_values
                .entry(format!("{range:.2}"))
                .or_default() += 1;
            let z = vfx.z_correct();
            if z == 0.0 {
                tally.z_correct_zero += 1;
            }
            *tally.z_correct_values.entry(format!("{z:.3}")).or_default() += 1;
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).expect("the path list");
    let mut tally = Tally::default();

    for path in list.lines().filter(|path| path.ends_with(".lvb")) {
        let Ok(level) = ironworks.file::<lvb::LevelFile>(path) else {
            continue;
        };
        let scene = level.scene();
        for group in scene.layer_groups() {
            visit(group, &mut tally, path);
        }
        for lgb_path in scene.layer_group_paths() {
            if let Ok(file) = ironworks.file::<ironworks::file::lgb::LayerGroupFile>(lgb_path) {
                visit(file.group(), &mut tally, path);
            }
        }
    }

    println!("Vfx instances with a non-empty asset_path: {}", tally.total);
    println!(
        "  auto_play: {} ({:.1}%)",
        tally.auto_play,
        pct(tally.auto_play, tally.total)
    );
    println!(
        "  no_far_clip: {} ({:.1}%)",
        tally.no_far_clip,
        pct(tally.no_far_clip, tally.total)
    );
    println!(
        "  non-white colour: {} ({:.1}%), alpha==0: {} ({:.1}%)",
        tally.non_white,
        pct(tally.non_white, tally.total),
        tally.alpha_zero,
        pct(tally.alpha_zero, tally.total)
    );
    println!("  most common rgba tuples:");
    let mut rgba: Vec<_> = tally.rgba_counts.iter().collect();
    rgba.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (tuple, count) in rgba.iter().take(10) {
        println!("    {tuple:?}: {count}");
    }
    println!("  non-white examples:");
    for example in &tally.examples_non_white {
        println!("    {example}");
    }
    println!(
        "  fade_near[0] <= fade_near[1]: {} ({:.1}%)",
        tally.near_ordered,
        pct(tally.near_ordered, tally.total)
    );
    println!(
        "  fade_far[0] <= fade_far[1]: {} ({:.1}%)",
        tally.far_ordered,
        pct(tally.far_ordered, tally.total)
    );
    println!(
        "    of which no_far_clip=false (clip active, n={}): {} ({:.1}%) ordered",
        tally.clip_total,
        tally.far_ordered_clip,
        pct(tally.far_ordered_clip, tally.clip_total)
    );
    println!(
        "    of which no_far_clip=true (clip inactive, n={}): {} ({:.1}%) ordered",
        tally.no_clip_total,
        tally.far_ordered_no_clip,
        pct(tally.far_ordered_no_clip, tally.no_clip_total)
    );
    println!(
        "  fade_near[1] <= fade_far[0]: {} ({:.1}%), of which exactly equal: {} ({:.1}%)",
        tally.near_before_far,
        pct(tally.near_before_far, tally.total),
        tally.near_touches_far,
        pct(tally.near_touches_far, tally.total)
    );
    println!("  bad-order examples (near[1] > far[0]):");
    for example in &tally.examples_bad_order {
        println!("    {example}");
    }
    println!(
        "  soft_particle_fade_range == 0: {} ({:.1}%)",
        tally.soft_particle_zero,
        pct(tally.soft_particle_zero, tally.total)
    );
    println!("  soft_particle_fade_range distinct values: {}", tally.soft_particle_values.len());
    for (value, count) in &tally.soft_particle_values {
        println!("    {value}: {count}");
    }
    println!(
        "  z_correct == 0: {} ({:.1}%)",
        tally.z_correct_zero,
        pct(tally.z_correct_zero, tally.total)
    );
    println!("  z_correct distinct values: {}", tally.z_correct_values.len());
    let mut z_correct: Vec<_> = tally.z_correct_values.iter().collect();
    z_correct.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (value, count) in z_correct.iter().take(20) {
        println!("    {value}: {count}");
    }
}

fn pct(part: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    part as f64 / total as f64 * 100.0
}
