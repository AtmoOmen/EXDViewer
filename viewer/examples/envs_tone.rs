//! What a zone states for the tone mapping and colour filter sets, against the values a capture of
//! the running game read out of its buffers.

use ironworks::file::{envb, lvb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const ZONES: [&str; 4] = [
    "sea_s1/fld/s1f1",
    "roc_r1/fld/r1f1",
    "wil_w1/fld/w1f1",
    "ffxiv/sea_s1/twn/s1t1",
];

fn shown(value: &ironworks::file::envs::Value) -> String {
    match value {
        ironworks::file::envs::Value::Float(held) => format!("{held:.6}"),
        ironworks::file::envs::Value::Unsigned(held) => held.to_string(),
        ironworks::file::envs::Value::Flag(held) => held.to_string(),
        ironworks::file::envs::Value::Colour(held) => format!(
            "{} {} {} x{:.3}",
            held.red(),
            held.green(),
            held.blue(),
            held.intensity()
        ),
        ironworks::file::envs::Value::Path(held) => held.clone(),
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let asked: Vec<String> = std::env::args().skip(1).collect();
    let zones = match asked.is_empty() {
        true => ZONES.iter().map(|held| (*held).to_owned()).collect(),
        false => asked,
    };
    for zone in zones {
        let stem = zone.rsplit('/').next().unwrap_or(&zone);
        let path = match zone.starts_with("ffxiv/") || zone.starts_with("ex") {
            true => format!("bg/{zone}/level/{stem}.lvb"),
            false => format!("bg/ffxiv/{zone}/level/{stem}.lvb"),
        };
        let level: lvb::LevelFile = match ironworks.file(&path) {
            Ok(held) => held,
            Err(why) => {
                println!("== {zone}: {why}\n");
                continue;
            }
        };
        let mut paths: Vec<String> = Vec::new();
        for env in level.scene().environments() {
            let held = env.asset_path();
            if !held.is_empty() && !paths.contains(held) {
                paths.push(held.clone());
            }
        }
        println!("== {zone}  {} environments", paths.len());
        for held in paths {
            let file: envb::EnvironmentFile = match ironworks.file(&held) {
                Ok(file) => file,
                Err(why) => {
                    println!("   {held}: {why}");
                    continue;
                }
            };
            println!("   {held}");
            for weather in file.environments().weathers() {
                for set in weather.sets() {
                    let wanted: u32 = std::env::var("SET")
                        .ok()
                        .and_then(|held| held.parse().ok())
                        .unwrap_or(9);
                    if set.kind() != wanted {
                        continue;
                    }
                    for keyframe in set.keyframes() {
                        let fields: Vec<String> = keyframe
                            .fields()
                            .iter()
                            .map(|(name, value)| format!("{name}={}", shown(value)))
                            .collect();
                        println!(
                            "      weather {:>4} set {:>2} at {:>7.0}  {}",
                            weather.id(),
                            set.kind(),
                            keyframe.time(),
                            fields.join("  ")
                        );
                    }
                }
            }
        }
        println!();
    }
}
