//! What an equipment set states about the body under it, and what the body calls the parts it hides.
//!
//! `eqp_parts attrs <path.mdl>`       the attributes a model declares, and where each part reaches
//! `eqp_parts census <prefix> [n]`    every attribute name across the models under a path prefix
//! `eqp_parts set <id>`               the flags one set states
//! `eqp_parts sweep <slot> [n]`       how often each combination of a slot's flags is stated
//! `eqp_parts reach <suffix> [n]`     a set's flags against how far its own model reaches
//! `eqp_parts split <suffix> <attr>`  the same, splitting a model at one attribute

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use ironworks::file::mdl::{VertexAttribute, VertexAttributeKind, VertexValues};
use ironworks::file::{File, eqp, mdl::ModelContainer};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const EQP: &str = "chara/xls/equipmentparameter/equipmentparameter.eqp";

fn names(ironworks: &Ironworks<SqPack<Install>>, path: &str) -> Option<Vec<(String, Vec<String>)>> {
    let bytes: Vec<u8> = match ironworks.file(path) {
        Ok(bytes) => bytes,
        Err(why) => {
            println!("  {path}: {why}");
            return None;
        }
    };
    let container = match ModelContainer::read(Cursor::new(bytes)) {
        Ok(container) => container,
        Err(why) => {
            println!("  {path}: {why}");
            return None;
        }
    };
    let model = container.model(ironworks::file::mdl::Lod::High);
    let declared = model.attribute_names().ok()?;
    let mut found = Vec::new();
    for (index, mesh) in model.meshes().into_iter().enumerate() {
        let held = mesh.attributes().ok();
        let positions = held.as_ref().and_then(|held| {
            held.iter()
                .find(|attribute| matches!(attribute.kind, VertexAttributeKind::Position))
                .and_then(|attribute: &VertexAttribute| match &attribute.values {
                    VertexValues::Vector3(values) => {
                        Some(values.iter().map(|held| [held[0], held[1], held[2]]).collect::<Vec<_>>())
                    }
                    VertexValues::Vector4(values) => {
                        Some(values.iter().map(|held| [held[0], held[1], held[2]]).collect::<Vec<_>>())
                    }
                    _ => None,
                })
        });
        let indices = mesh.indices();
        for part in mesh.submeshes() {
            let claimed: Vec<String> = (0..declared.len())
                .filter(|bit| part.attributes & 1 << bit != 0)
                .map(|bit| declared[bit].clone())
                .collect();
            let span = match (&positions, &indices) {
                (Some(positions), Ok(indices)) => {
                    let held: Vec<f32> = indices[part.start..part.start + part.count]
                        .iter()
                        .filter_map(|at| positions.get(usize::from(*at)).map(|held| held[1]))
                        .collect();
                    let low = held.iter().copied().fold(f32::MAX, f32::min);
                    let high = held.iter().copied().fold(f32::MIN, f32::max);
                    format!("y {low:6.3}..{high:6.3}")
                }
                _ => String::new(),
            };
            found.push((
                format!("mesh {index} x{:5} {span}", part.count),
                claimed,
            ));
        }
    }
    Some(found)
}

fn extent(ironworks: &Ironworks<SqPack<Install>>, path: &str) -> Option<(f32, f32)> {
    let bytes: Vec<u8> = ironworks.file(path).ok()?;
    let container = ModelContainer::read(Cursor::new(bytes)).ok()?;
    let model = container.model(ironworks::file::mdl::Lod::High);
    let mut low = f32::MAX;
    let mut high = f32::MIN;
    for mesh in model.meshes() {
        for attribute in mesh.attributes().ok()? {
            if !matches!(attribute.kind, VertexAttributeKind::Position) {
                continue;
            }
            let values: Vec<f32> = match &attribute.values {
                VertexValues::Vector3(values) => values.iter().map(|held| held[1]).collect(),
                VertexValues::Vector4(values) => values.iter().map(|held| held[1]).collect(),
                _ => continue,
            };
            low = low.min(values.iter().copied().fold(f32::MAX, f32::min));
            high = high.max(values.iter().copied().fold(f32::MIN, f32::max));
        }
    }
    (low < high).then_some((low, high))
}

fn every() -> Vec<&'static str> {
    vec![
        "body.hide_waist",
        "body.hide_thighs",
        "body.hide_gloves_small",
        "body.hide_glove_cuffs",
        "body.hide_gloves_medium",
        "body.hide_gloves_large",
        "body.hide_gorget",
        "legs.hide_knee_pads",
        "legs.hide_boots_small",
        "legs.hide_boots_medium",
        "hands.hide_elbow",
        "hands.hide_forearm",
        "feet.hide_knee",
        "feet.hide_calf",
        "feet.hide_ankle",
        "head.hide_neck",
    ]
}

fn flags(set: &eqp::Set) -> Vec<&'static str> {
    let mut on = Vec::new();
    let body = set.body();
    for (name, held) in [
        ("body.enabled", body.enabled()),
        ("body.hide_waist", body.hide_waist()),
        ("body.hide_thighs", body.hide_thighs()),
        ("body.hide_gloves_small", body.hide_gloves_small()),
        ("body.hide_glove_cuffs", body.hide_glove_cuffs()),
        ("body.hide_gloves_medium", body.hide_gloves_medium()),
        ("body.hide_gloves_large", body.hide_gloves_large()),
        ("body.hide_gorget", body.hide_gorget()),
        ("body.show_legs", body.show_legs()),
        ("body.show_hands", body.show_hands()),
        ("body.show_head", body.show_head()),
        ("body.show_necklace", body.show_necklace()),
        ("body.show_bracelets", body.show_bracelets()),
        ("body.show_tail", body.show_tail()),
    ] {
        if held {
            on.push(name);
        }
    }
    let legs = set.legs();
    for (name, held) in [
        ("legs.enabled", legs.enabled()),
        ("legs.hide_knee_pads", legs.hide_knee_pads()),
        ("legs.hide_boots_small", legs.hide_boots_small()),
        ("legs.hide_boots_medium", legs.hide_boots_medium()),
        ("legs.show_feet", legs.show_feet()),
        ("legs.show_tail", legs.show_tail()),
    ] {
        if held {
            on.push(name);
        }
    }
    let hands = set.hands();
    for (name, held) in [
        ("hands.enabled", hands.enabled()),
        ("hands.hide_elbow", hands.hide_elbow()),
        ("hands.hide_forearm", hands.hide_forearm()),
        ("hands.show_bracelets", hands.show_bracelets()),
        ("hands.show_ring_left", hands.show_ring_left()),
        ("hands.show_ring_right", hands.show_ring_right()),
    ] {
        if held {
            on.push(name);
        }
    }
    let feet = set.feet();
    for (name, held) in [
        ("feet.enabled", feet.enabled()),
        ("feet.hide_knee", feet.hide_knee()),
        ("feet.hide_calf", feet.hide_calf()),
        ("feet.hide_ankle", feet.hide_ankle()),
    ] {
        if held {
            on.push(name);
        }
    }
    let head = set.head();
    for (name, held) in [
        ("head.enabled", head.enabled()),
        ("head.hide_scalp", head.hide_scalp()),
        ("head.hide_hair", head.hide_hair()),
        ("head.show_hair_override", head.show_hair_override()),
        ("head.hide_neck", head.hide_neck()),
        ("head.show_necklace", head.show_necklace()),
        ("head.show_ears_human", head.show_ears_human()),
        ("head.show_ears_miqote", head.show_ears_miqote()),
        ("head.show_ears_au_ra", head.show_ears_au_ra()),
        ("head.show_ears_viera", head.show_ears_viera()),
    ] {
        if held {
            on.push(name);
        }
    }
    on
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = arguments.first().map(String::as_str).unwrap_or("");
    match command {
        "attrs" => {
            let path = &arguments[1];
            match names(&ironworks, path) {
                Some(parts) => {
                    for (where_, claimed) in parts {
                        println!("  {where_:24} {}", claimed.join(", "));
                    }
                }
                None => println!("  {path}: unreadable"),
            }
        }
        "census" => {
            let prefix = &arguments[1];
            let mut tally: BTreeMap<String, usize> = BTreeMap::new();
            for id in 0..=arguments
                .get(2)
                .and_then(|held| held.parse::<u16>().ok())
                .unwrap_or(200)
            {
                let path = prefix.replace("NNNN", &format!("{id:04}"));
                let Some(parts) = names(&ironworks, &path) else {
                    continue;
                };
                for name in parts.into_iter().flat_map(|(_, claimed)| claimed) {
                    *tally.entry(name).or_default() += 1;
                }
            }
            for (name, count) in tally {
                println!("  {name:16} {count}");
            }
        }
        "set" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            let id: u16 = arguments[1].parse().unwrap();
            println!("  set {id}: {}", flags(&file.set(id)).join(" "));
        }
        "sweep" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            let slot = arguments[1].as_str();
            let count: u16 = arguments
                .get(2)
                .and_then(|held| held.parse().ok())
                .unwrap_or(400);
            let mut tally: BTreeMap<String, usize> = BTreeMap::new();
            for id in 1..count {
                let held = file.set(id);
                let on: BTreeSet<&str> = flags(&held)
                    .into_iter()
                    .filter(|name| name.starts_with(slot))
                    .collect();
                *tally
                    .entry(on.into_iter().collect::<Vec<_>>().join(" "))
                    .or_default() += 1;
            }
            for (on, count) in tally {
                println!("  {count:5}  {on}");
            }
        }
        "split" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            let suffix = arguments[1].as_str();
            let gate = arguments[2].as_str();
            let count: u16 = arguments
                .get(3)
                .and_then(|held| held.parse().ok())
                .unwrap_or(1000);
            let mut spread: BTreeMap<(&str, bool), Vec<(f32, f32)>> = BTreeMap::new();
            for id in 1..count {
                let path = format!("chara/equipment/e{id:04}/model/c0101e{id:04}_{suffix}.mdl");
                let Some(parts) = names(&ironworks, &path) else {
                    continue;
                };
                let mut base = f32::MIN;
                let mut gated = f32::MIN;
                for (span, claimed) in &parts {
                    let Some(high) = span
                        .rsplit("..")
                        .next()
                        .and_then(|held| held.trim().parse::<f32>().ok())
                    else {
                        continue;
                    };
                    match claimed.iter().any(|name| name == gate) {
                        true => gated = gated.max(high),
                        false => base = base.max(high),
                    }
                }
                let on: BTreeSet<&str> = flags(&file.set(id)).into_iter().collect();
                for name in every() {
                    spread
                        .entry((name, on.contains(name)))
                        .or_default()
                        .push((base, gated));
                }
            }
            for ((name, held), values) in spread {
                if values.len() < 4 {
                    continue;
                }
                let mut base: Vec<f32> = values.iter().map(|(held, _)| *held).collect();
                let mut gated: Vec<f32> = values
                    .iter()
                    .map(|(_, held)| *held)
                    .filter(|held| *held > f32::MIN)
                    .collect();
                base.sort_by(f32::total_cmp);
                gated.sort_by(f32::total_cmp);
                println!(
                    "  {name:26} {:3} {:5}  base {:6.3} {:6.3} {:6.3}   {gate} {:4} {:6.3} {:6.3} {:6.3}",
                    match held { true => "on", false => "off" },
                    base.len(),
                    base[0], base[base.len() / 2], base[base.len() - 1],
                    gated.len(),
                    gated.first().copied().unwrap_or(0.0),
                    gated.get(gated.len() / 2).copied().unwrap_or(0.0),
                    gated.last().copied().unwrap_or(0.0),
                );
            }
        }
        "reach" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            let suffix = arguments[1].as_str();
            let count: u16 = arguments
                .get(2)
                .and_then(|held| held.parse().ok())
                .unwrap_or(400);
            let mut spread: BTreeMap<(&str, bool), Vec<f32>> = BTreeMap::new();
            for id in 1..count {
                let path =
                    format!("chara/equipment/e{id:04}/model/c0101e{id:04}_{suffix}.mdl");
                let Some((low, high)) = extent(&ironworks, &path) else {
                    continue;
                };
                let held = file.set(id);
                let on: BTreeSet<&str> = flags(&held).into_iter().collect();
                for name in every() {
                    spread
                        .entry((name, on.contains(name)))
                        .or_default()
                        .push(match suffix {
                            "glv" | "sho" => high,
                            _ => low,
                        });
                }
            }
            for ((name, held), mut values) in spread {
                values.sort_by(f32::total_cmp);
                if values.len() < 4 {
                    continue;
                }
                println!(
                    "  {name:26} {:5}  {:5} {:6.3}  {:6.3}  {:6.3}  {:6.3}  {:6.3}",
                    match held { true => "on", false => "off" },
                    values.len(),
                    values[0],
                    values[values.len() / 10],
                    values[values.len() / 2],
                    values[values.len() * 9 / 10],
                    values[values.len() - 1],
                );
            }
        }
        _ => println!("attrs | census | set | sweep | reach"),
    }
}
