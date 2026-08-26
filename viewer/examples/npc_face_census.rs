//! How many `ENpcBase` rows state a Face or Hairstyle id with no model on disk for the tribe they
//! resolve to. `character::mod::pick` falls back to the lowest id it finds rather than the stated
//! one whenever this happens, silently substituting a face or hairstyle. See `npc-face-tribe-
//! mismatch` in memory.
//!
//! `npc_face_census`

use std::collections::BTreeMap;
use std::sync::Arc;

use ironworks::excel::{Excel, Field, Language, Row};
use ironworks::file::exh::ColumnDefinition;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

// Byte offsets into `ENpcBase`'s `Customize` array, matching `character::npcs`.
const CUSTOMIZE: u16 = 202;
const RACE: u16 = CUSTOMIZE;
const GENDER: u16 = CUSTOMIZE + 1;
const TRIBE: u16 = CUSTOMIZE + 4;
const FACE: u16 = CUSTOMIZE + 5;
const HAIRSTYLE: u16 = CUSTOMIZE + 6;
const SINGULAR: u16 = 0;

/// A part built the way `character::mod::sets` names its files: `obj/<dir>/<letter>####/model/
/// c####<letter>####_<suffix>.mdl`.
struct Part {
    offset: u16,
    dir: &'static str,
    letter: char,
    suffix: &'static str,
}
const FACE_PART: Part = Part { offset: FACE, dir: "face", letter: 'f', suffix: "fac" };
const HAIR_PART: Part = Part { offset: HAIRSTYLE, dir: "hair", letter: 'h', suffix: "hir" };

/// Which body a tribe is built on; see `character::mod::BUILT_ON`.
const BUILT_ON: [u16; 16] = [1, 3, 5, 5, 11, 11, 7, 7, 9, 9, 13, 13, 15, 15, 17, 17];

fn resolve(tribe: u32, female: bool) -> u16 {
    let body = BUILT_ON.get(tribe.max(1) as usize - 1).copied().unwrap_or(1);
    (body + u16::from(female)) * 100 + 1
}

/// The column at a known byte offset, found by offset rather than by exh index: the exh's own
/// column order is not offset order, so an index guessed from the schema would name the wrong one.
fn column(columns: &[ColumnDefinition], offset: u16) -> ColumnDefinition {
    columns
        .iter()
        .find(|column| column.offset() == offset)
        .cloned()
        .unwrap_or_else(|| panic!("no column at offset {offset}"))
}

fn int(row: &Row, column: &ColumnDefinition) -> u32 {
    match row.field(column) {
        Ok(Field::U8(v)) => u32::from(v),
        Ok(Field::I8(v)) => u32::try_from(v).unwrap_or(0),
        Ok(Field::U16(v)) => u32::from(v),
        Ok(Field::Bool(v)) => u32::from(v),
        _ => 0,
    }
}

fn main() {
    let ironworks: Arc<Ironworks> = Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);

    let bases = excel.sheet("ENpcBase").expect("ENpcBase");
    let residents = excel.sheet("ENpcResident").expect("ENpcResident");
    let columns = bases.columns().expect("ENpcBase columns");
    let name_column = column(&residents.columns().expect("ENpcResident columns"), SINGULAR);
    let (race_col, gender_col, tribe_col, face_col, hair_col) = (
        column(&columns, RACE),
        column(&columns, GENDER),
        column(&columns, TRIBE),
        column(&columns, FACE_PART.offset),
        column(&columns, HAIR_PART.offset),
    );

    let mut exists: BTreeMap<(char, u16, u32), bool> = BTreeMap::new();
    let mut check = |ironworks: &Ironworks, part: &Part, code: u16, id: u32| -> bool {
        *exists.entry((part.letter, code, id)).or_insert_with(|| {
            let letter = part.letter;
            let suffix = part.suffix;
            let path = format!(
                "chara/human/c{code:04}/obj/{}/{letter}{id:04}/model/c{code:04}{letter}{id:04}_{suffix}.mdl",
                part.dir
            );
            ironworks.file::<Vec<u8>>(&path).is_ok()
        })
    };

    let (mut total, mut face_missing, mut hair_missing) = (0usize, 0usize, 0usize);
    let mut by_tribe: BTreeMap<u32, (usize, usize, usize, Vec<String>)> = BTreeMap::new();

    for row in bases.into_iter() {
        let race = int(&row, &race_col);
        let tribe = int(&row, &tribe_col);
        if race == 0 || tribe == 0 {
            continue;
        }
        let Ok(Field::String(name)) = residents
            .row(row.row_id())
            .and_then(|held| held.field(&name_column))
        else {
            continue;
        };
        let name = name.to_string();
        if name.is_empty() {
            continue;
        }

        let gender = int(&row, &gender_col);
        let (face, hair) = (int(&row, &face_col), int(&row, &hair_col));
        let code = resolve(tribe, gender != 0);
        let face_ok = check(&ironworks, &FACE_PART, code, face);
        let hair_ok = check(&ironworks, &HAIR_PART, code, hair);

        total += 1;
        let tally = by_tribe.entry(tribe).or_default();
        tally.0 += 1;
        if !face_ok {
            face_missing += 1;
            tally.1 += 1;
            if tally.3.len() < 6 {
                tally.3.push(format!("{name} (face {face}, c{code:04})"));
            }
        }
        if !hair_ok {
            hair_missing += 1;
            tally.2 += 1;
        }
    }

    println!("{face_missing}/{total} named ENpcBase rows state a face with no model for their tribe");
    println!("{hair_missing}/{total} state a hairstyle with no model for their tribe");
    for (tribe, (count, bad_face, bad_hair, examples)) in by_tribe {
        if bad_face > 0 || bad_hair > 0 {
            println!(
                "  tribe {tribe}: {bad_face}/{count} face, {bad_hair}/{count} hair missing — {}",
                examples.join(", ")
            );
        }
    }
}
