//! What a zone's placed lights state for themselves, against the box each is clipped to. The viewer
//! takes its falloff from the box; these are the fields it does not read.

use ironworks::file::{layer, lcb, lgb::LayerGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn walk(held: &layer::LayerGroup, into: &mut Vec<(u32, f32, f32, f32, [f32; 3])>) {
    for layer in held.layers() {
        for instance in layer.instances() {
            if let layer::InstanceData::Light(light) = instance.data() {
                into.push((
                    instance.id(),
                    light.range(),
                    light.attenuation(),
                    light.spot_angle(),
                    instance.transform().translation(),
                ));
            }
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let zone = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bg/ex1/01_roc_r2/twn/r2t1".to_owned());

    let mut lights = Vec::new();
    for name in ["bg", "planmap", "planlive", "planevent"] {
        let path = format!("{zone}/level/{name}.lgb");
        if let Ok(file) = ironworks.file::<LayerGroupFile>(&path) {
            walk(file.group(), &mut lights);
        }
    }
    println!("{} lights placed", lights.len());

    let stem = zone.rsplit('/').next().unwrap_or(&zone);
    let boxes = ironworks
        .file::<lcb::ClipBoxes>(&format!("{zone}/level/{stem}.lcb"))
        .ok();
    let mut clipped = 0usize;
    if let Some(held) = &boxes {
        for group in held.groups() {
            clipped += group.entries().len();
        }
    }
    println!("{clipped} clip box entries\n");

    let mut zero = 0usize;
    for (id, range, atten, cone, at) in lights.iter().take(40) {
        let span = ((at[0] + 64.0).powi(2) + (at[1] - 9.5).powi(2) + (at[2] - 44.0).powi(2)).sqrt();
        println!(
            "  #{id:<9} range {range:>8.3}  atten {atten:>7.3}  cone {cone:>7.3}  at ({:.1}, {:.1}, {:.1})  {span:.1} from the aetheryte",
            at[0], at[1], at[2],
        );
        if *range <= 0.0 {
            zero += 1;
        }
    }
    let all_zero = lights.iter().filter(|held| held.1 <= 0.0).count();
    println!(
        "\n{all_zero} of {} lights state a range of nought or less ({zero} in the sample above)",
        lights.len()
    );
    let mut ranges: Vec<f32> = lights.iter().map(|held| held.1).collect();
    ranges.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !ranges.is_empty() {
        println!(
            "range: min {:.3}  median {:.3}  max {:.3}",
            ranges[0],
            ranges[ranges.len() / 2],
            ranges[ranges.len() - 1]
        );
    }
}
