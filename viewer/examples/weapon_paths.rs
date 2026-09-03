//! Every weapon and shield the `Item` sheet names, resolved to the files it draws from and checked
//! against the install: the main-hand model, the off-hand model, a fist weapon's gauntlets, and the
//! material each one's `.imc` sends it to.
//!
//! `weapon_paths [--list-fists] [--list-misses]`

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ironworks::excel::{Excel, Field, Language, Row};
use ironworks::file::exh::ColumnDefinition;
use ironworks::file::mdl::{Lod, ModelContainer};
use ironworks::file::{File, imc};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const NAME: u16 = 12;
const MODEL_MAIN: u16 = 24;
const MODEL_SUB: u16 = 32;
const SLOT_CATEGORY: u16 = 154;

/// The set range `DrawDataContainer::LoadWeapon` reads an off-hand model as the main's plus fifty
/// over, and `LoadEquipment` reads the item's `ModelSub` as a Hands equipment id for.
const FISTS: std::ops::RangeInclusive<u16> = 1601..=1650;

/// The races a piece of equipment can be filed under, nearest first: nothing but the model's own
/// existence is checked here, so a set that ships only one race still counts as present.
const CODES: [u16; 4] = [101, 201, 301, 401];

fn column(columns: &[ColumnDefinition], offset: u16) -> ColumnDefinition {
    columns
        .iter()
        .find(|column| column.offset() == offset)
        .cloned()
        .unwrap_or_else(|| panic!("no column at offset {offset}"))
}

fn int(row: &Row, column: &ColumnDefinition) -> u64 {
    match row.field(column) {
        Ok(Field::U8(v)) => u64::from(v),
        Ok(Field::U16(v)) => u64::from(v),
        Ok(Field::U32(v)) => u64::from(v),
        Ok(Field::U64(v)) => v,
        _ => 0,
    }
}

/// The file a material name points at, the way `mdl::material::path` builds it: the name spells
/// out its own directory, which is not always the model's own, since an off-hand knuckle names the
/// main hand's material.
fn material_path(model: &str, name: &str, variant: u16) -> Option<String> {
    let name = name.trim_start_matches('/');
    let worn = variant.max(1);
    if let Some((set, rest)) = model
        .strip_prefix("chara/weapon/w")
        .and_then(|tail| tail.split_once("/obj/body/b"))
    {
        let set: u32 = set.parse().ok()?;
        let base: u32 = rest.get(..4)?.parse().ok()?;
        if set / 100 == 20 && name.as_bytes().get(14) == Some(&b'c') {
            return Some(
                "chara/weapon/w2001/obj/body/b0001/material/v0001/mt_w2001b0001_c.mtrl".to_owned(),
            );
        }
        let shared = shared_set(set);
        let name = match shared == set {
            true => name.to_owned(),
            false => format!("mt_w{shared:04}{}", name.strip_prefix("mt_")?.get(5..)?),
        };
        return Some(format!(
            "chara/weapon/w{shared:04}/obj/body/b{base:04}/material/v{worn:04}/{name}"
        ));
    }
    let stem = name.strip_prefix("mt_")?;
    let kind = stem.as_bytes().first().copied()? as char;
    let set = stem.as_bytes().get(5).copied()? as char;
    let body: u32 = stem.get(1..5)?.parse().ok()?;
    let part: u32 = stem.get(6..10)?.parse().ok()?;
    let directory = match (kind, set) {
        ('c', 'b') => format!("chara/human/c{body:04}/obj/body/b{part:04}/material/v0001"),
        ('c', 'e') => format!("chara/equipment/e{part:04}/material/v{worn:04}"),
        _ => return None,
    };
    Some(format!("{directory}/{name}"))
}

/// `Weapon::ResolveMtrlPath` and `ResolveImcPath` file the off-hand half of a paired weapon under
/// the main hand's set.
fn shared_set(set: u32) -> u32 {
    const PAIRED: [u32; 6] = [3, 16, 18, 26, 30, 31];
    match PAIRED.contains(&(set / 100)) && set % 100 > 50 {
        true => set - 50,
        false => set,
    }
}

fn weapon_model(set: u16, base: u16) -> String {
    format!("chara/weapon/w{set:04}/obj/body/b{base:04}/model/w{set:04}b{base:04}.mdl")
}

fn glove_model(code: u16, set: u16) -> String {
    format!("chara/equipment/e{set:04}/model/c{code:04}e{set:04}_glv.mdl")
}

struct Probe {
    ironworks: Arc<Ironworks>,
    imcs: BTreeMap<String, Option<imc::ImageChange>>,
}

impl Probe {
    /// Whether the model is on disk, how many bones it skins to, and which of the materials it
    /// names at the variant its `.imc` resolves are missing.
    fn check(&mut self, model: &str, part: u8, variant: u16) -> Option<(usize, Vec<String>)> {
        let container = self.ironworks.file::<ModelContainer>(model).ok()?;
        let high = container.model(Lod::High);
        let bones = high.bone_names().map(|held| held.len()).unwrap_or(0);
        let base = &model[..model.rfind("/model/")?];
        let stem = base.rsplit('/').next()?;
        let base = match base.strip_prefix("chara/weapon/w") {
            Some(tail) => format!(
                "chara/weapon/w{:04}{}",
                shared_set(tail.get(..4)?.parse().ok()?),
                &tail[4..]
            ),
            None => base.to_owned(),
        };
        let resolved = match self.imc(&format!("{base}/{stem}.imc")) {
            Some(image_change) => image_change
                .entry(part, variant)
                .map_or(variant, |entry| u16::from(entry.material_id())),
            None => variant,
        };
        let mut missing = Vec::new();
        let mut asked = BTreeSet::new();
        for mesh in high.meshes() {
            let Ok(material) = mesh.material() else {
                continue;
            };
            let Some(path) = material_path(model, &material, resolved) else {
                continue;
            };
            if asked.insert(path.clone()) && self.ironworks.file::<Vec<u8>>(&path).is_err() {
                missing.push(format!("{path} for {model}"));
            }
        }
        Some((bones, missing))
    }

    fn imc(&mut self, path: &str) -> Option<&imc::ImageChange> {
        if !self.imcs.contains_key(path) {
            let held = self
                .ironworks
                .file::<Vec<u8>>(path)
                .ok()
                .and_then(|bytes| imc::ImageChange::read(std::io::Cursor::new(bytes)).ok());
            self.imcs.insert(path.to_owned(), held);
        }
        self.imcs[path].as_ref()
    }

    fn exists(&self, path: &str) -> bool {
        self.ironworks.file::<Vec<u8>>(path).is_ok()
    }
}

fn main() {
    let flags: BTreeSet<String> = std::env::args().skip(1).collect();
    let ironworks: Arc<Ironworks> =
        Arc::new(Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))));
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);
    let items = excel.sheet("Item").expect("Item");
    let columns = items.columns().expect("Item columns");
    let (name, slot) = (column(&columns, NAME), column(&columns, SLOT_CATEGORY));
    let (main, sub) = (column(&columns, MODEL_MAIN), column(&columns, MODEL_SUB));

    // One entry per distinct quad pair, since thousands of items share a model.
    let mut wielded: BTreeMap<(u64, u64), String> = BTreeMap::new();
    for row in items.into_iter() {
        if !matches!(int(&row, &slot), 1 | 2 | 13 | 14) {
            continue;
        }
        let quad = int(&row, &main);
        if quad == 0 {
            continue;
        }
        let Ok(Field::String(held)) = row.field(&name) else {
            continue;
        };
        let held = held.to_string();
        if held.is_empty() {
            continue;
        }
        wielded.entry((quad, int(&row, &sub))).or_insert(held);
    }

    let mut probe = Probe {
        ironworks,
        imcs: BTreeMap::new(),
    };
    let (mut mains, mut offs, mut gloves) = (0u32, 0u32, 0u32);
    let (mut main_miss, mut off_miss, mut glove_miss) = (Vec::new(), Vec::new(), Vec::new());
    let mut material_miss: Vec<String> = Vec::new();
    let mut skinned: Vec<String> = Vec::new();
    let mut fists: Vec<String> = Vec::new();
    let mut old_off_miss = 0u32;

    for ((quad, sub), held) in &wielded {
        let (set, base, variant) = (*quad as u16, (quad >> 16) as u16, (quad >> 32) as u16);
        let fist = FISTS.contains(&set);
        let model = weapon_model(set, base);
        mains += 1;
        match probe.check(&model, 0, variant) {
            None => main_miss.push(format!("{held}: {model}")),
            Some((bones, missing)) => {
                if bones > 1 {
                    skinned.push(format!("{held}: {model} skins {bones} bones"));
                }
                material_miss.extend(missing.into_iter().map(|path| format!("{held}: {path}")));
            }
        }

        if fist {
            fists.push(format!("{held}: w{set:04}b{base:04}v{variant} sub {sub:#x}"));
        }
        let off = match fist {
            true => Some((set + 50, base, variant)),
            false => (*sub != 0).then(|| (*sub as u16, (sub >> 16) as u16, (sub >> 32) as u16)),
        };
        if let Some((set, base, variant)) = off {
            offs += 1;
            let model = weapon_model(set, base);
            match probe.check(&model, 0, variant) {
                None => off_miss.push(format!("{held}: {model}")),
                Some((bones, missing)) => {
                    if bones > 1 {
                        skinned.push(format!("{held}: {model} skins {bones} bones"));
                    }
                    material_miss.extend(missing.into_iter().map(|path| format!("{held}: {path}")));
                }
            }
        }
        // What the viewer resolved before the fist range was known: `ModelSub` read as a weapon.
        if *sub != 0 && !probe.exists(&weapon_model(*sub as u16, (sub >> 16) as u16)) {
            old_off_miss += 1;
        }

        if fist && *sub != 0 {
            gloves += 1;
            let (set, variant) = (*sub as u16, (sub >> 16) as u8);
            let model = CODES
                .iter()
                .map(|code| glove_model(*code, set))
                .find(|path| probe.exists(path));
            match model {
                None => glove_miss.push(format!("{held}: {}", glove_model(101, set))),
                Some(model) => {
                    if let Some((_, missing)) = probe.check(&model, 2, u16::from(variant)) {
                        material_miss
                            .extend(missing.into_iter().map(|path| format!("{held}: {path}")));
                    }
                }
            }
        }
    }

    println!("{} distinct wielded model pairs", wielded.len());
    println!("main hand: {mains} resolved, {} missing", main_miss.len());
    println!("off hand:  {offs} resolved, {} missing", off_miss.len());
    println!("gauntlets: {gloves} resolved, {} missing", glove_miss.len());
    println!("materials: {} missing", material_miss.len());
    println!("fist range {FISTS:?}: {} pairs", fists.len());
    println!("off hand read as a weapon quad throughout: {old_off_miss} missing");
    println!("models skinning more than one bone: {}", skinned.len());
    let show = |title: &str, held: &[String]| {
        println!("\n== {title}");
        for line in held.iter().take(60) {
            println!("  {line}");
        }
        if held.len() > 60 {
            println!("  ... {} more", held.len() - 60);
        }
    };
    show("main hand misses", &main_miss);
    show("off hand misses", &off_miss);
    show("gauntlet misses", &glove_miss);
    show("material misses", &material_miss);
    if flags.contains("--list-fists") {
        show("fist range", &fists);
    }
    if flags.contains("--list-skinned") {
        show("skinned", &skinned);
    }
}
