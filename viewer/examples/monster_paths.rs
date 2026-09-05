//! Every monster body `ModelChara` names, resolved to the files it draws from and checked against
//! the install: the model, the material colourway its own `.imc` sends it to, and whether the
//! materials it names are there in that colourway and in the base one.
//!
//! `monster_paths [--list-misses]`

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

/// `ModelChara`'s model, kind, base and variant, as byte offsets.
const MODEL: u16 = 12;
const KIND: u16 = 16;
const BASE: u16 = 17;
const VARIANT: u16 = 18;

/// The `ModelChara.Type` a monster body is filed under.
const MONSTER: u64 = 3;

fn column(columns: &[ColumnDefinition], offset: u16) -> ColumnDefinition {
    columns
        .iter()
        .find(|column| column.offset() == offset)
        .cloned()
        .unwrap_or_else(|| panic!("no column at offset {offset}"))
}

fn int(row: &Row, column: &ColumnDefinition) -> u64 {
    match row.field(column) {
        Ok(Field::U8(value)) => u64::from(value),
        Ok(Field::U16(value)) => u64::from(value),
        Ok(Field::U32(value)) => u64::from(value),
        Ok(Field::U64(value)) => value,
        _ => 0,
    }
}

struct Probe {
    ironworks: Arc<Ironworks>,
    imcs: BTreeMap<String, Option<imc::ImageChange>>,
}

impl Probe {
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
    let sheet = excel.sheet("ModelChara").expect("ModelChara");
    let columns = sheet.columns().expect("ModelChara columns");
    let (model, kind) = (column(&columns, MODEL), column(&columns, KIND));
    let (base, variant) = (column(&columns, BASE), column(&columns, VARIANT));

    // One entry per distinct body and variant, since several rows name the same one.
    let mut bodies: BTreeSet<(u64, u64, u64)> = BTreeSet::new();
    for row in sheet.into_iter() {
        if int(&row, &kind) == MONSTER {
            bodies.insert((int(&row, &model), int(&row, &base), int(&row, &variant)));
        }
    }

    let mut probe = Probe {
        ironworks,
        imcs: BTreeMap::new(),
    };
    let (mut models, mut model_miss) = (0u32, Vec::new());
    // The colourway an entry sends the body to, against the base folder alone.
    let (mut fixed, mut lost, mut base_miss) = (Vec::new(), Vec::new(), Vec::new());
    // An entry naming material nought, which is the game drawing no material at all.
    let mut unlit: Vec<String> = Vec::new();
    // A variant the imc carries no entry for, which the game reads past the end of the file for.
    let mut unstated: Vec<String> = Vec::new();
    // A default entry naming anything but the base colourway.
    let mut moved_default: Vec<String> = Vec::new();

    for (monster, body, asked) in &bodies {
        let directory = format!("chara/monster/m{monster:04}/obj/body/b{body:04}");
        let model = format!("{directory}/model/m{monster:04}b{body:04}.mdl");
        let Ok(container) = probe.ironworks.file::<ModelContainer>(&model) else {
            model_miss.push(model);
            continue;
        };
        models += 1;
        let asked = u16::try_from(*asked).unwrap_or_default();
        let image_change = probe.imc(&format!("{directory}/b{body:04}.imc"));
        let carried = image_change.map(imc::ImageChange::variant_count);
        let entry = image_change.and_then(|held| held.entry(0, asked));
        let default = image_change.and_then(|held| held.entry(0, 0));
        if default.is_some_and(|entry| entry.material_id() != 1) {
            moved_default.push(format!("m{monster:04}b{body:04}"));
        }
        let resolved = match entry {
            Some(entry) => u16::from(entry.material_id()),
            None => {
                if let Some(carried) = carried {
                    unstated.push(format!(
                        "m{monster:04}b{body:04} variant {asked}, the imc carries {carried}"
                    ));
                }
                asked
            }
        };
        if resolved == 0 {
            unlit.push(format!("m{monster:04}b{body:04} variant {asked}"));
            continue;
        }
        let mut asked_for = BTreeSet::new();
        for mesh in container.model(Lod::High).meshes() {
            let Ok(name) = mesh.material() else { continue };
            let name = name.trim_start_matches('/').to_owned();
            if !asked_for.insert(name.clone()) {
                continue;
            }
            let at = |worn: u16| format!("{directory}/material/v{worn:04}/{name}");
            if resolved == 1 {
                if !probe.exists(&at(1)) {
                    base_miss.push(at(1));
                }
            } else if probe.exists(&at(resolved)) {
                fixed.push(at(resolved));
            } else if probe.exists(&at(1)) {
                lost.push(format!("{}, though v0001 is there", at(resolved)));
            } else {
                base_miss.push(at(1));
            }
        }
    }

    println!("{} monster bodies named, {models} models read", bodies.len());
    println!("materials the imc colourway reaches: {}", fixed.len());
    println!("materials it misses that v0001 holds: {}", lost.len());
    println!("materials missing at v0001 too: {}", base_miss.len());
    println!("entries naming no material at all: {}", unlit.len());
    println!("variants the imc states nothing for: {}", unstated.len());
    println!("default entries past the base colourway: {}", moved_default.len());
    let show = |title: &str, held: &[String]| {
        println!("\n== {title}");
        for line in held.iter().take(40) {
            println!("  {line}");
        }
        if held.len() > 40 {
            println!("  ... {} more", held.len() - 40);
        }
    };
    show("missed, though v0001 is there", &lost);
    show("no material stated", &unlit);
    show("variants the imc states nothing for", &unstated);
    show("defaults past the base colourway", &moved_default);
    if flags.contains("--list-misses") {
        show("model misses", &model_miss);
        show("v0001 misses", &base_miss);
    }
}
