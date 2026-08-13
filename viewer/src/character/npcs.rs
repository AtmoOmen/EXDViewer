//! The characters the game itself stands in the world, out of `ENpcBase` and `ENpcResident`.
//!
//! A base row carries the whole of what the creator would have picked, in the creator's own
//! numbering, plus a model quad per slot. The resident row of the same id is what it is called.

use anyhow::Result;
use ironworks::excel::Language;

use super::Gear;
use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelSheet};

/// What `ENpcResident` calls a base row.
const SINGULAR: u32 = 0;

/// Where `ENpcBase` states the body it is built on, then everything the creator would have picked,
/// as byte offsets. Every one of them is a byte, and every colour is a palette index outright.
const RACE: u32 = 202;
const GENDER: u32 = 203;
const TRIBE: u32 = 206;
/// The customisations, paired with the `Customize` each drives.
const PICKED: [(u32, u32); 15] = [
    (205, 3),  // Height
    (207, 5),  // Face
    (208, 6),  // Hairstyle
    (210, 8),  // Skin colour
    (212, 10), // Hair colour
    (214, 12), // Facial features
    (215, 13), // Tattoo colour
    (216, 14), // Eyebrows
    (217, 9),  // Eye colour
    (218, 16), // Eye shape
    (219, 17), // Nose
    (220, 18), // Jaw
    (221, 19), // Mouth
    (222, 20), // Lip colour
    (227, 25), // Face paint colour
];
/// The model quad worn in each of `Slot::ALL`, packed as the set in the low half and the variant
/// in the high one.
const MODELS: [u32; 5] = [148, 152, 156, 160, 164];

/// One of the game's own characters, as far as building it goes.
pub struct Npc {
    pub name: String,
    pub race: u32,
    pub tribe: u32,
    pub female: bool,
    /// What each of the creator's menus was left at, by the `Customize` it drives.
    pub choices: Vec<(u32, u32)>,
    pub outfit: [Option<Gear>; 5],
}

/// Every named character the game builds out of a human body. The unnamed ones are left out: a
/// list of sixty thousand rows is not something to search by name.
pub async fn read(backend: &Backend, language: Language) -> Result<Vec<Npc>> {
    let excel = backend.excel();
    let bases = excel.get_sheet("ENpcBase", language).await?;
    let residents = excel.get_sheet("ENpcResident", language).await?;

    let mut found = Vec::new();
    for id in bases.get_row_ids() {
        let Ok(row) = bases.get_row(id) else {
            continue;
        };
        let (Ok(race), Ok(tribe), Ok(gender)) = (
            row.read::<u8>(RACE),
            row.read::<u8>(TRIBE),
            row.read::<u8>(GENDER),
        ) else {
            continue;
        };
        if race == 0 || tribe == 0 {
            continue;
        }
        let name = residents
            .get_row(id)
            .ok()
            .and_then(|held| held.read_string(SINGULAR).ok().map(|name| name.to_string()))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let mut outfit = [None; 5];
        for (slot, at) in MODELS.into_iter().enumerate() {
            outfit[slot] = row
                .read::<u32>(at)
                .ok()
                .filter(|quad| *quad != u32::MAX)
                .and_then(|quad| Gear::read(u64::from(quad)));
        }
        found.push(Npc {
            name,
            race: u32::from(race),
            tribe: u32::from(tribe),
            female: gender != 0,
            choices: PICKED
                .into_iter()
                .filter_map(|(at, customize)| {
                    Some((customize, u32::from(row.read::<u8>(at).ok()?)))
                })
                .collect(),
            outfit,
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!("character: {} named characters to stand in", found.len());
    Ok(found)
}
