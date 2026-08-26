//! Two checks against real `ENpcBase` data: whether the Dye/Dye2 byte offsets this crate now reads
//! (`character::npcs`) line up with the model quads they dye, and how many named rows carry a
//! nonzero dye — the population `character::npcs::read` silently dropped before it read them at
//! all. Also counts how often an imc's `material_id` for a worn quad's `(part, variant)` differs
//! from the variant itself, which is the population the unindirected `v{variant:04}` material path
//! would 404 on.
//!
//! `npc_dye_census`

use std::collections::BTreeMap;
use std::sync::Arc;

use ironworks::excel::{Excel, Field, Language, Row};
use ironworks::file::exh::ColumnDefinition;
use ironworks::file::{File, imc};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const RACE: u16 = 202;
const TRIBE: u16 = 206;
const SINGULAR: u16 = 0;

const SLOTS: [&str; 10] = [
    "Head",
    "Body",
    "Hands",
    "Legs",
    "Feet",
    "Ears",
    "Neck",
    "Wrists",
    "LeftRing",
    "RightRing",
];
const MODEL_AT: [u16; 10] = [148, 152, 156, 160, 164, 168, 172, 176, 180, 184];
const DYE_AT: [u16; 10] = [233, 234, 235, 236, 237, 238, 239, 240, 241, 242];
const DYE2_AT: [u16; 10] = [243, 244, 245, 246, 247, 248, 249, 250, 251, 252];
/// Which imc part a slot's quad reads: head/ears 0, body/neck 1, hands/wrists 2, legs/right ring 3,
/// feet/left ring 4, matching `mdl::mod::imc_part`.
const IMC_PART: [u8; 10] = [0, 1, 2, 3, 4, 0, 1, 2, 4, 3];
/// Whether a slot's set is filed under `chara/equipment` (`e`) or `chara/accessory` (`a`).
const ADORNMENT: [bool; 10] = [
    false, false, false, false, false, true, true, true, true, true,
];

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
        Ok(Field::U16(v)) => u32::from(v),
        Ok(Field::U32(v)) => v,
        Ok(Field::I8(v)) => u32::try_from(v).unwrap_or(0),
        Ok(Field::Bool(v)) => u32::from(v),
        _ => 0,
    }
}

fn main() {
    let ironworks: Arc<Ironworks> =
        Arc::new(Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))));
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);

    let bases = excel.sheet("ENpcBase").expect("ENpcBase");
    let residents = excel.sheet("ENpcResident").expect("ENpcResident");
    let columns = bases.columns().expect("ENpcBase columns");
    let name_column = column(
        &residents.columns().expect("ENpcResident columns"),
        SINGULAR,
    );
    let (race_col, tribe_col) = (column(&columns, RACE), column(&columns, TRIBE));
    let model_cols: Vec<ColumnDefinition> =
        MODEL_AT.iter().map(|&at| column(&columns, at)).collect();
    let dye_cols: Vec<ColumnDefinition> = DYE_AT.iter().map(|&at| column(&columns, at)).collect();
    let dye2_cols: Vec<ColumnDefinition> = DYE2_AT.iter().map(|&at| column(&columns, at)).collect();

    let mut total = 0usize;
    let mut dyed_rows = 0usize;
    let mut dye_bytes = 0usize;
    let mut invariant_breaks = 0usize;
    let mut dye_on_zero_examples: Vec<String> = Vec::new();

    let mut imc_cache: BTreeMap<(bool, u32), Option<imc::ImageChange>> = BTreeMap::new();
    let mut quads_seen = 0usize;
    let mut mismatched = 0usize;
    let mut mismatch_examples: Vec<String> = Vec::new();

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
        total += 1;

        let mut row_dyed = false;
        for slot in 0..10 {
            let quad = int(&row, &model_cols[slot]);
            let dye = int(&row, &dye_cols[slot]);
            let dye2 = int(&row, &dye2_cols[slot]);
            if dye != 0 {
                dye_bytes += 1;
                row_dyed = true;
            }
            if dye2 != 0 {
                dye_bytes += 1;
                row_dyed = true;
            }
            if quad == 0 && (dye != 0 || dye2 != 0) {
                invariant_breaks += 1;
                if dye_on_zero_examples.len() < 8 {
                    dye_on_zero_examples.push(format!(
                        "{name} slot {} dye={dye} dye2={dye2} with no model",
                        SLOTS[slot]
                    ));
                }
            }
            if quad == 0 {
                continue;
            }
            let set = quad as u16;
            let variant = (quad >> 16) as u16;
            if variant == 0 {
                continue;
            }
            quads_seen += 1;
            let key = (ADORNMENT[slot], u32::from(set));
            let image_change = imc_cache.entry(key).or_insert_with(|| {
                let kind = if ADORNMENT[slot] { 'a' } else { 'e' };
                let dir = if ADORNMENT[slot] {
                    "accessory"
                } else {
                    "equipment"
                };
                let path = format!("chara/{dir}/{kind}{set:04}/{kind}{set:04}.imc");
                ironworks
                    .file::<Vec<u8>>(&path)
                    .ok()
                    .and_then(|bytes| imc::ImageChange::read(std::io::Cursor::new(bytes)).ok())
            });
            let Some(image_change) = image_change else {
                continue;
            };
            let Some(entry) = image_change.entry(IMC_PART[slot], variant) else {
                continue;
            };
            let material_id = entry.material_id();
            if u16::from(material_id) != variant {
                mismatched += 1;
                if mismatch_examples.len() < 8 {
                    mismatch_examples.push(format!(
                        "{name} slot {} set {set} variant {variant} -> material_id {material_id}",
                        SLOTS[slot]
                    ));
                }
            }
        }
        if row_dyed {
            dyed_rows += 1;
        }
    }

    println!("{total} named ENpcBase rows examined");
    println!(
        "{dyed_rows}/{total} rows carry a nonzero dye byte in at least one slot ({dye_bytes} nonzero dye bytes total across both channels)"
    );
    println!("invariant check (zero model quad implies zero dye): {invariant_breaks} breaks");
    for example in &dye_on_zero_examples {
        println!("  {example}");
    }
    println!();
    println!(
        "{mismatched}/{quads_seen} worn (set, variant) quads resolve to a different imc material_id than the raw variant"
    );
    for example in &mismatch_examples {
        println!("  {example}");
    }
}
