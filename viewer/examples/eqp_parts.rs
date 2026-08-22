//! What an equipment set states about the body under it, and what the body calls the parts it hides.
//!
//! `eqp_parts attrs <path.mdl>`       the attributes a model declares, and where each part reaches
//! `eqp_parts census <prefix> [n]`    every attribute name across the models under a path prefix
//! `eqp_parts set <id>`               the flags one set states
//! `eqp_parts sweep <slot> [n]`       how often each combination of a slot's flags is stated
//! `eqp_parts reach <suffix> [n]`     a set's flags against how far its own model reaches
//! `eqp_parts split <suffix> <attr>`  the same, splitting a model at one attribute
//! `eqp_parts imc <path.mdl> <part> <path.imc>`  which of a model's parts each variant shows

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
                format!("mesh {index} [{}] x{:5} {span}", mesh.material().unwrap_or_default(), part.count),
                claimed,
            ));
        }
    }
    Some(found)
}

/// Every part of a model, as the attributes it declares and the box its own vertices fill.
fn spans(
    ironworks: &Ironworks<SqPack<Install>>,
    path: &str,
) -> Option<Vec<(Vec<String>, [f32; 4], usize)>> {
    let bytes: Vec<u8> = ironworks.file(path).ok()?;
    let container = ModelContainer::read(Cursor::new(bytes)).ok()?;
    let model = container.model(ironworks::file::mdl::Lod::High);
    let declared = model.attribute_names().ok()?;
    let mut found = Vec::new();
    for mesh in model.meshes() {
        let Some(positions) = mesh.attributes().ok().and_then(|held| {
            held.into_iter()
                .find(|attribute| matches!(attribute.kind, VertexAttributeKind::Position))
                .and_then(|attribute| match attribute.values {
                    VertexValues::Vector3(values) => {
                        Some(values.iter().map(|held| [held[0], held[1]]).collect::<Vec<_>>())
                    }
                    VertexValues::Vector4(values) => {
                        Some(values.iter().map(|held| [held[0], held[1]]).collect::<Vec<_>>())
                    }
                    _ => None,
                })
        }) else {
            continue;
        };
        let Ok(indices) = mesh.indices() else { continue };
        for part in mesh.submeshes() {
            let claimed: Vec<String> = (0..declared.len())
                .filter(|bit| part.attributes & 1 << bit != 0)
                .map(|bit| declared[bit].clone())
                .collect();
            let mut box_ = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
            for at in &indices[part.start..part.start + part.count] {
                let Some(held) = positions.get(usize::from(*at)) else {
                    continue;
                };
                box_[0] = box_[0].min(held[1]);
                box_[1] = box_[1].max(held[1]);
                box_[2] = box_[2].min(held[0].abs());
                box_[3] = box_[3].max(held[0].abs());
            }
            if box_[0] <= box_[1] {
                found.push((claimed, box_, part.count));
            }
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
        ("head.show_earrings_hyur_roegadyn", head.show_earrings_hyur_roegadyn()),
        ("head.show_earrings_elezen_lalafell", head.show_earrings_elezen_lalafell()),
        ("head.show_earrings_miqote_hrothgar_viera", head.show_earrings_miqote_hrothgar_viera()),
        ("head.show_earrings_au_ra", head.show_earrings_au_ra()),
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
                        println!("  {where_:70} {}", claimed.join(", "));
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
        "imc" => {
            let path = &arguments[1];
            let part: u8 = arguments[2].parse().unwrap();
            let bytes: Vec<u8> = ironworks.file(&arguments[3]).unwrap();
            let file = ironworks::file::imc::ImageChange::read(Cursor::new(bytes)).unwrap();
            let held: Vec<u8> = ironworks.file(path).unwrap();
            let container = ModelContainer::read(Cursor::new(held)).unwrap();
            let declared = container
                .model(ironworks::file::mdl::Lod::High)
                .attribute_names()
                .unwrap();
            for variant in 0..=file.variant_count() {
                let Some(entry) = file.entry(part, variant) else {
                    continue;
                };
                let mask = u32::from(entry.attribute_mask());
                let shown: Vec<&str> = declared
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| mask & 1 << bit != 0)
                    .map(|(_, name)| name.as_str())
                    .collect();
                println!("  variant {variant}: mask {mask:#012b} shows {}", shown.join(" "));
            }
        }
        "raw" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let control = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            let count: u16 = arguments
                .get(1)
                .and_then(|held| held.parse().ok())
                .unwrap_or(1200);
            for id in 1..count {
                let block = id / 160;
                let entry = match control & 1 << block != 0 {
                    true => {
                        let index = 160 * (control & ((1 << block) - 1)).count_ones() as usize
                            + usize::from(id % 160);
                        bytes
                            .get(index * 8..index * 8 + 8)
                            .map_or(0x3fe00070603f00, |held| {
                                u64::from_le_bytes(held.try_into().unwrap())
                            })
                    }
                    false => 0x3fe00070603f00,
                };
                println!("{id},{entry:016x}");
            }
        }
        "dump" => {
            let count: u16 = arguments
                .get(1)
                .and_then(|held| held.parse().ok())
                .unwrap_or(1200);
            println!("set,kind,attr,ylo,yhi,xlo,xhi,n");
            for id in 1..count {
                for kind in ["top", "glv", "dwn", "sho", "met"] {
                    let path = format!("chara/equipment/e{id:04}/model/c0101e{id:04}_{kind}.mdl");
                    let Some(parts) = spans(&ironworks, &path) else {
                        continue;
                    };
                    for (claimed, box_, n) in parts {
                        let names = match claimed.is_empty() {
                            true => String::from("-"),
                            false => claimed.join("+"),
                        };
                        println!(
                            "{id},{kind},{names},{:.4},{:.4},{:.4},{:.4},{n}",
                            box_[0], box_[1], box_[2], box_[3]
                        );
                    }
                }
            }
        }
        "bits" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            let count: u16 = arguments
                .get(1)
                .and_then(|held| held.parse().ok())
                .unwrap_or(1200);
            for id in 0..count {
                println!("{id},{}", flags(&file.set(id)).join(" "));
            }
        }
        "legs" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            println!("set,knee,calf,ankle,sho,dwn_hiz,dwn_sne,dwn_lpd");
            for id in 1..arguments.get(1).and_then(|h| h.parse::<u16>().ok()).unwrap_or(1200) {
                let sho = format!("chara/equipment/e{id:04}/model/c0101e{id:04}_sho.mdl");
                let dwn = format!("chara/equipment/e{id:04}/model/c0101e{id:04}_dwn.mdl");
                let held = file.set(id);
                let feet = held.feet();
                if !feet.enabled() {
                    continue;
                }
                let high = |parts: &Option<Vec<(String, Vec<String>)>>, want: Option<&str>| -> f32 {
                    let Some(parts) = parts else { return -1.0 };
                    let mut top = -1.0f32;
                    for (span, claimed) in parts {
                        let matched = match want {
                            Some(name) => claimed.iter().any(|held| held == name),
                            None => claimed.is_empty(),
                        };
                        if !matched {
                            continue;
                        }
                        if let Some(value) = span.rsplit("..").next().and_then(|h| h.trim().parse::<f32>().ok()) {
                            top = top.max(value);
                        }
                    }
                    top
                };
                let sho_parts = names(&ironworks, &sho);
                let dwn_parts = names(&ironworks, &dwn);
                if sho_parts.is_none() {
                    continue;
                }
                let boot = high(&sho_parts, None).max(high(&sho_parts, Some("atr_leg")));
                println!(
                    "{id},{},{},{},{:.3},{:.3},{:.3},{:.3}",
                    u8::from(feet.hide_knee()),
                    u8::from(feet.hide_calf()),
                    u8::from(feet.hide_ankle()),
                    boot,
                    high(&dwn_parts, Some("atr_hiz")),
                    high(&dwn_parts, Some("atr_sne")),
                    high(&sho_parts, Some("atr_lpd")),
                );
            }
        }
        "arms" => {
            let bytes: Vec<u8> = ironworks.file(EQP).unwrap();
            let file = eqp::EquipmentParameter::read(Cursor::new(bytes)).unwrap();
            println!("set,elbow,forearm,glv_base,glv_arm,top_hij,top_ude");
            for id in 1..arguments.get(1).and_then(|h| h.parse::<u16>().ok()).unwrap_or(1200) {
                let glv = format!("chara/equipment/e{id:04}/model/c0101e{id:04}_glv.mdl");
                let top = format!("chara/equipment/e{id:04}/model/c0101e{id:04}_top.mdl");
                let held = file.set(id);
                let hands = held.hands();
                if !hands.enabled() {
                    continue;
                }
                let high = |parts: &Option<Vec<(String, Vec<String>)>>, want: Option<&str>| -> f32 {
                    let Some(parts) = parts else { return -1.0 };
                    let mut top = -1.0f32;
                    for (span, claimed) in parts {
                        let matched = match want {
                            Some(name) => claimed.iter().any(|held| held == name),
                            None => claimed.is_empty(),
                        };
                        if !matched {
                            continue;
                        }
                        if let Some(value) = span.rsplit("..").next().and_then(|h| h.trim().parse::<f32>().ok()) {
                            top = top.max(value);
                        }
                    }
                    top
                };
                let glv_parts = names(&ironworks, &glv);
                let top_parts = names(&ironworks, &top);
                if glv_parts.is_none() {
                    continue;
                }
                println!(
                    "{id},{},{},{:.3},{:.3},{:.3},{:.3}",
                    u8::from(hands.hide_elbow()),
                    u8::from(hands.hide_forearm()),
                    high(&glv_parts, None),
                    high(&glv_parts, Some("atr_arm")),
                    high(&top_parts, Some("atr_hij")),
                    high(&top_parts, Some("atr_ude")),
                );
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
        _ => println!("attrs | census | set | sweep | reach | dump | bits"),
    }
}
