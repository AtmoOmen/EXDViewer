//! Set 13 ("Vertical fog") of a zone's environment timeline, with the colour alpha byte the
//! generic dump drops, and the keyframe pair straddling a given time interpolated.

use ironworks::file::{envb, lvb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn shown(value: &ironworks::file::envs::Value) -> String {
    match value {
        ironworks::file::envs::Value::Float(held) => format!("{held:.6}"),
        ironworks::file::envs::Value::Unsigned(held) => held.to_string(),
        ironworks::file::envs::Value::Flag(held) => held.to_string(),
        ironworks::file::envs::Value::Colour(held) => format!(
            "{} {} {} a{} x{:.3}",
            held.red(),
            held.green(),
            held.blue(),
            held.alpha(),
            held.intensity()
        ),
        ironworks::file::envs::Value::Path(held) => held.clone(),
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let zone = args.next().expect("zone");
    let wanted: u32 = args.next().and_then(|held| held.parse().ok()).unwrap_or(1);
    let at: f32 = args
        .next()
        .and_then(|held| held.parse().ok())
        .unwrap_or(0.0);
    let stem = zone.rsplit('/').next().unwrap_or(&zone).to_owned();
    let path = match zone.starts_with("ffxiv/") || zone.starts_with("ex") {
        true => format!("bg/{zone}/level/{stem}.lvb"),
        false => format!("bg/ffxiv/{zone}/level/{stem}.lvb"),
    };
    let level: lvb::LevelFile = ironworks.file(&path).unwrap();
    let mut paths: Vec<String> = Vec::new();
    for env in level.scene().environments() {
        let held = env.asset_path();
        if !held.is_empty() && !paths.contains(held) {
            paths.push(held.clone());
        }
    }
    for held in paths {
        let file: envb::EnvironmentFile = ironworks.file(&held).unwrap();
        for weather in file.environments().weathers() {
            if weather.id() != wanted {
                continue;
            }
            for set in weather.sets() {
                if set.kind() != 13 {
                    continue;
                }
                let frames: Vec<_> = set.keyframes().iter().collect();
                for keyframe in &frames {
                    let fields: Vec<String> = keyframe
                        .fields()
                        .iter()
                        .map(|(name, value)| format!("{name}={}", shown(value)))
                        .collect();
                    println!(
                        "weather {} at {:>7.0}  {}",
                        weather.id(),
                        keyframe.time(),
                        fields.join("  ")
                    );
                }
                let pair = frames
                    .windows(2)
                    .find(|two| two[0].time() <= at && at <= two[1].time());
                if let Some(two) = pair {
                    let span = two[1].time() - two[0].time();
                    let u = (at - two[0].time()) / span;
                    println!(
                        "\nat {at} : u = {u:.6} between {} and {}",
                        two[0].time(),
                        two[1].time()
                    );
                    for (name, value) in two[0].fields() {
                        let after = two[1].fields().iter().find(|(other, _)| other == name);
                        if let (
                            ironworks::file::envs::Value::Colour(low),
                            Some((_, ironworks::file::envs::Value::Colour(high))),
                        ) = (value, after)
                        {
                            let mix =
                                |a: u8, b: u8| f32::from(a) + (f32::from(b) - f32::from(a)) * u;
                            println!(
                                "   {name} = {:.6} {:.6} {:.6} a{:.6}   /255 = {:.9} {:.9} {:.9}",
                                mix(low.red(), high.red()),
                                mix(low.green(), high.green()),
                                mix(low.blue(), high.blue()),
                                mix(low.alpha(), high.alpha()),
                                mix(low.red(), high.red()) / 255.0,
                                mix(low.green(), high.green()) / 255.0,
                                mix(low.blue(), high.blue()) / 255.0,
                            );
                        }
                    }
                }
            }
        }
    }
}
