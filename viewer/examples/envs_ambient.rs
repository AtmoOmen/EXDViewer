//! What a zone states for its ambient light: the global lighting set of every weather, and the
//! harmonics the `.amb` its `EnvLocation` names holds.

use ironworks::file::{amb, envb, layer::InstanceData, lgb, lvb};
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
            "{} {} {} x{:.6}",
            held.red(),
            held.green(),
            held.blue(),
            held.intensity()
        ),
        ironworks::file::envs::Value::Path(held) => held.clone(),
    }
}

fn main() {
    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let zone = std::env::args().nth(1).expect("zone");
    if zone == "sky" {
        let amb::Ambient::SkyLight(held) = ironworks
            .file::<amb::Ambient>("bgcommon/nature/sky/ambient/skylight.amb")
            .unwrap()
        else {
            panic!("skylight is not a sky light")
        };
        for sky in held.skies() {
            println!("sky {}  {} samples", sky.id(), sky.count());
            for (at, sample) in held.samples(sky.id()).unwrap().iter().enumerate() {
                println!(
                    "  s={at}\n    r={:?}\n    g={:?}\n    b={:?}",
                    sample.red(),
                    sample.green(),
                    sample.blue()
                );
            }
        }
        return;
    }
    if zone.ends_with(".envb") {
        let file: envb::EnvironmentFile = ironworks.file(&zone).unwrap();
        println!("weathers {}", file.environments().weathers().len());
        for weather in file.environments().weathers() {
            println!(
                " weather {} sets {:?}",
                weather.id(),
                weather
                    .sets()
                    .iter()
                    .map(|s| (s.kind(), s.keyframes().len()))
                    .collect::<Vec<_>>()
            );
            for set in weather.sets() {
                if set.kind() != 0 {
                    continue;
                }
                for keyframe in set.keyframes() {
                    let fields: Vec<String> = keyframe
                        .fields()
                        .iter()
                        .map(|(name, value)| format!("{name}={}", shown(value)))
                        .collect();
                    println!(
                        "   weather {:>4} set {:>2} at {:>7.0}  {}",
                        weather.id(),
                        set.kind(),
                        keyframe.time(),
                        fields.join("  ")
                    );
                }
            }
        }
        return;
    }
    if zone == "scan" {
        let excel = ironworks::excel::Excel::new(ironworks.clone());
        let sheet = excel.sheet("TerritoryType").unwrap();
        let mut seen: Vec<String> = Vec::new();
        for row in 0..2000u32 {
            let Ok(held) = sheet.row(row) else { continue };
            let bg = match held.field(1) {
                Ok(ironworks::excel::Field::String(bg)) => bg.to_string(),
                other => {
                    if row < 5 {
                        println!("row {row} field 1 = {other:?}");
                    }
                    continue;
                }
            };
            if bg.is_empty() || seen.contains(&bg) {
                continue;
            }
            seen.push(bg.clone());
            let Ok(level) = ironworks.file::<lvb::LevelFile>(&format!("bg/{bg}.lvb")) else {
                continue;
            };
            let scene = level.scene();
            let mut report = |group: &ironworks::file::layer::LayerGroup| {
                for layer in group.layers() {
                    for instance in layer.instances() {
                        if let InstanceData::EnvSpace(held) = instance.data() {
                            let at = instance.transform();
                            println!(
                                "{bg} envspace {} at {:?} rot {:?} scale {:?} shape {:?} range {} {}",
                                instance.id(),
                                at.translation(),
                                at.rotation(),
                                at.scale(),
                                held.shape(),
                                held.effective_range(),
                                held.asset_path()
                            );
                        }
                    }
                }
            };
            for group in scene.layer_groups() {
                report(group);
            }
            for held in scene.layer_group_paths() {
                if let Ok(file) = ironworks.file::<lgb::LayerGroupFile>(held) {
                    report(file.group());
                }
            }
        }
        return;
    }
    let want: Option<u32> = std::env::args().nth(2).and_then(|held| held.parse().ok());
    let stem = zone.rsplit('/').next().unwrap_or(&zone).to_owned();
    let path = match zone.starts_with("ffxiv/") || zone.starts_with("ex") {
        true => format!("bg/{zone}/level/{stem}.lvb"),
        false => format!("bg/ffxiv/{zone}/level/{stem}.lvb"),
    };
    let level: lvb::LevelFile = ironworks.file(&path).unwrap();
    let scene = level.scene();

    let mut wanted = Vec::new();
    for env in scene.environments() {
        println!(
            "environment  envb={}  instance={}",
            env.asset_path(),
            env.env_location_instance_id()
        );
        wanted.push((
            env.asset_path().clone(),
            env.env_location_instance_id() as u32,
        ));
    }

    let mut located: Vec<(u32, String)> = Vec::new();
    let mut visit = |group: &ironworks::file::layer::LayerGroup| {
        for layer in group.layers() {
            for instance in layer.instances() {
                if let InstanceData::EnvLocation(held) = instance.data() {
                    let at = instance.transform();
                    println!(
                        "envloc {} at {:?} rot {:?} scale {:?}  layer {}  {}",
                        instance.id(),
                        at.translation(),
                        at.rotation(),
                        at.scale(),
                        layer.name(),
                        held.env_map_asset_path()
                    );
                    located.push((instance.id(), held.ambient_light_asset_path().clone()));
                }
                if let InstanceData::EnvSpace(held) = instance.data() {
                    let at = instance.transform();
                    println!(
                        "envspace {} at {:?} scale {:?} shape {:?} range {} bound {}  {}",
                        instance.id(),
                        at.translation(),
                        at.scale(),
                        held.shape(),
                        held.effective_range(),
                        held.bound_instance_id(),
                        held.asset_path()
                    );
                }
            }
        }
    };
    for group in scene.layer_groups() {
        visit(group);
    }
    for held in scene.layer_group_paths() {
        if let Ok(file) = ironworks.file::<lgb::LayerGroupFile>(held) {
            visit(file.group());
        }
    }

    for (id, path) in &located {
        println!("located instance {id}  {path}");
    }

    for (instance, amb_path) in located.clone() {
        let envb_path = wanted
            .iter()
            .find(|(_, id)| *id == instance)
            .map_or_else(|| wanted[0].0.clone(), |(path, _)| path.clone());
        let envb_path = &envb_path;
        println!("\n=== instance {instance}  amb={amb_path:?}");
        {
            let amb_path = &amb_path;
            match ironworks.file::<amb::Ambient>(&amb_path) {
                Ok(amb::Ambient::EnvLocation(held)) => {
                    println!("sky_visibility {:?}", held.sky_visibility());
                    for track in 0..amb::TRACK_COUNT {
                        let Some(keyframes) = held.track(track) else {
                            continue;
                        };
                        if keyframes.is_empty() {
                            continue;
                        }
                        println!("track {track}  {} keyframes", keyframes.len());
                        for keyframe in keyframes {
                            let light = keyframe.light();
                            println!(
                                "  t={:>8.1}\n    r={:?}\n    g={:?}\n    b={:?}",
                                keyframe.time(),
                                light.red(),
                                light.green(),
                                light.blue()
                            );
                        }
                    }
                }
                Ok(other) => println!("not an env location: {other:?}"),
                Err(why) => println!("{amb_path}: {why}"),
            }
        }

        println!("\n--- {envb_path}");
        let file: envb::EnvironmentFile = match ironworks.file(envb_path) {
            Ok(file) => file,
            Err(why) => {
                println!("   {why}");
                continue;
            }
        };
        for weather in file.environments().weathers() {
            if want.is_some_and(|id| id != weather.id()) {
                continue;
            }
            for set in weather.sets() {
                if set.kind() != 0 {
                    continue;
                }
                for keyframe in set.keyframes() {
                    let fields: Vec<String> = keyframe
                        .fields()
                        .iter()
                        .map(|(name, value)| format!("{name}={}", shown(value)))
                        .collect();
                    println!(
                        "   weather {:>4} set {:>2} at {:>7.0}  {}",
                        weather.id(),
                        set.kind(),
                        keyframe.time(),
                        fields.join("  ")
                    );
                }
            }
        }
    }
}
