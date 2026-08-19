//! The parameter each weather of a zone states, against the sky a capture was measured to bind.

use ironworks::file::{envb, lvb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for zone in std::env::args().skip(1) {
        let stem = zone.rsplit('/').next().unwrap_or(&zone).to_owned();
        let path = match zone.starts_with("ffxiv/") || zone.starts_with("ex") {
            true => format!("bg/{zone}/level/{stem}.lvb"),
            false => format!("bg/ffxiv/{zone}/level/{stem}.lvb"),
        };
        let level: lvb::LevelFile = match ironworks.file(&path) {
            Ok(held) => held,
            Err(why) => {
                println!("== {zone}: {why}");
                continue;
            }
        };
        let mut seen: Vec<String> = Vec::new();
        for env in level.scene().environments() {
            let held = env.asset_path();
            if !held.is_empty() && !seen.contains(held) {
                seen.push(held.clone());
            }
        }
        println!("== {zone}");
        for held in seen {
            let file: envb::EnvironmentFile = match ironworks.file(&held) {
                Ok(file) => file,
                Err(why) => {
                    println!("   {held}: {why}");
                    continue;
                }
            };
            for weather in file.environments().weathers() {
                let sky = format!(
                    "bgcommon/nature/sky/texture/sky_{:03}.tex",
                    weather.parameter()
                );
                let held: Result<Vec<u8>, _> = ironworks.file(&sky);
                println!(
                    "   weather {:>3}  parameter {:>4}  weight {:>6.2}  length {:>7.0}  {}  {}",
                    weather.id(),
                    weather.parameter(),
                    weather.weight(),
                    weather.length(),
                    match held {
                        Ok(bytes) => format!("{:>8} B", bytes.len()),
                        Err(_) => "       -".to_owned(),
                    },
                    weather.paths().join(" ").trim(),
                );
            }
        }
    }
}
