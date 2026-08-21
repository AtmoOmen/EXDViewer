//! The characters the game itself stands in the world, out of `ENpcBase` and `ENpcResident`.
//!
//! A base row carries the whole of what the creator would have picked, in the creator's own
//! numbering, plus a model quad per slot. The resident row of the same id is what it is called.

use anyhow::Result;
use ironworks::excel::Language;

use super::{Gear, HIGHLIGHT_COLOR, HIGHLIGHTS, LEFT_EYE_COLOR, LIPSTICK, ODD_EYES};
use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelSheet};

/// What `ENpcResident` calls a base row.
const SINGULAR: u32 = 0;

/// Where `ENpcBase` writes out the whole of what the creator would have picked. It is the game's
/// own customisation array laid down byte for byte, so a byte's place in it is also the menu the
/// creator drives it from, and everything below is that place rather than an offset into the row.
const CUSTOMIZE: u32 = 202;
const RACE: u32 = 0;
const GENDER: u32 = 1;
const TRIBE: u32 = 4;
/// The customisations stated as the menu's own position, which counts from one where a menu counts
/// from nought, each with the mask picking it out of a byte two menus share.
const LISTED: [(u32, u8); 5] = [
    (14, 0xFF), // Eyebrows
    (16, 0x7F), // Eye shape
    (17, 0xFF), // Nose
    (18, 0xFF), // Jaw
    (19, 0x7F), // Mouth
];
/// The ones stated outright: a palette index, a slider's own place, a mask of features, or the
/// number the file tree files a set under. A face and a hairstyle are the last of those and not a
/// position: the artillerist's face is 216, which is `f0216` on disk and a face no menu offers.
const STATED: [(u32, u8); 14] = [
    (3, 0xFF),  // Height
    (5, 0xFF),  // Face
    (6, 0xFF),  // Hairstyle
    (8, 0xFF),  // Skin colour
    (9, 0xFF),  // Eye colour
    (10, 0xFF), // Hair colour
    (12, 0xFF), // Facial features
    (13, 0xFF), // Tattoo colour
    (20, 0xFF), // Lip colour
    (21, 0xFF), // Muscle tone
    (22, 0xFF), // Tail or ear shape
    (23, 0xFF), // Bust size
    (24, 0x7F), // Face paint
    (25, 0xFF), // Face paint colour
];
/// The bytes the creator ticks a box for rather than offering a menu of its own, and the one it
/// shares with the eye colour: an eye is odd exactly where the two are not the same colour.
const HIGHLIGHTS_AT: u32 = 7;
const HIGHLIGHT_COLOR_AT: u32 = 11;
const LEFT_EYE_AT: u32 = 15;
const EYE_AT: u32 = 9;
/// Iris size, which is the top bit of the byte the eye shape menu holds the rest of. The creator
/// numbers its menu fifteen even though the byte of that number is the left eye's colour.
const IRIS_AT: u32 = 16;
const IRIS: u32 = 15;
/// Lipstick, which is the top bit of the byte the mouth menu holds the rest of.
const LIPSTICK_AT: u32 = 19;
/// The model quad worn in each of `Slot::ALL`, packed as the set in the low half and the variant
/// in the high one.
const MODELS: [u32; 10] = [148, 152, 156, 160, 164, 168, 172, 176, 180, 184];

/// One of the game's own characters, as far as building it goes.
pub struct Npc {
    pub name: String,
    pub race: u32,
    pub tribe: u32,
    pub female: bool,
    /// What each of the creator's menus was left at, by the `Customize` it drives.
    pub choices: Vec<(u32, u32)>,
    pub outfit: [Option<Gear>; 10],
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
        let byte = |at: u32| u32::from(row.read::<u8>(CUSTOMIZE + at).unwrap_or(0));
        let (race, tribe, gender) = (byte(RACE), byte(TRIBE), byte(GENDER));
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
        let mut outfit = [None; 10];
        for (slot, at) in MODELS.into_iter().enumerate() {
            outfit[slot] = row
                .read::<u32>(at)
                .ok()
                .filter(|quad| *quad != u32::MAX)
                .and_then(|quad| Gear::read(u64::from(quad)));
        }
        let (left, right) = (byte(LEFT_EYE_AT), byte(EYE_AT));
        found.push(Npc {
            name,
            race,
            tribe,
            female: gender != 0,
            choices: LISTED
                .into_iter()
                .map(|(at, mask)| (at, (byte(at) & u32::from(mask)).saturating_sub(1)))
                .chain(
                    STATED
                        .into_iter()
                        .map(|(at, mask)| (at, byte(at) & u32::from(mask))),
                )
                .chain([
                    (IRIS, byte(IRIS_AT) >> 7),
                    (LIPSTICK, byte(LIPSTICK_AT) >> 7),
                    (HIGHLIGHTS, byte(HIGHLIGHTS_AT) >> 7),
                    (HIGHLIGHT_COLOR, byte(HIGHLIGHT_COLOR_AT)),
                    (ODD_EYES, u32::from(left != right)),
                    (LEFT_EYE_COLOR, left),
                ])
                .collect(),
            outfit,
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!("character: {} named characters to stand in", found.len());
    Ok(found)
}
