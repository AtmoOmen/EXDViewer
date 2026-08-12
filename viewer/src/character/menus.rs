//! What the character creator itself offers, out of `CharaMakeType` and `CharaMakeCustomize`.
//!
//! A `CharaMakeType` row is one menu per customisation, each naming which it drives in `Customize`.
//! The two this reads state their choices differently: the face menu holds icon ids outright, and
//! the hair menu holds `CharaMakeCustomize` rows, which carry the set number alongside the icon.

use anyhow::Result;
use std::collections::BTreeMap;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelRow, ExcelSheet};

/// Menus a row holds, and the int32s one menu is.
const MENUS: u32 = 28;
const STRIDE: u32 = 452;
/// `Customize` and the first of the `SubMenuParam`s, as byte offsets into a menu.
const CUSTOMIZE: u32 = 8;
const PARAMS: u32 = 12;
const PARAM_COUNT: u32 = 90;

/// Which customisation a menu drives, as the creator's own numbering has it.
const FACE: i32 = 5;
const HAIR: i32 = 6;

/// The icon each set of a kind is offered under, by set number.
#[derive(Default)]
pub struct Icons {
    pub faces: BTreeMap<u16, u32>,
    pub hairs: BTreeMap<u16, u32>,
}

/// Reads the creator's menus for whichever of its rows offers the hair the code actually ships.
///
/// A model code does not name a clan, and two clans can share one, so the row is found by which of
/// them offers hair this code has rather than by matching a race up: the wrong row would offer
/// another race's hair under another race's icons.
pub async fn read(backend: &Backend, on_disk: &[u16]) -> Result<Icons> {
    let language = ironworks::excel::Language::English;
    let types = backend.excel().get_sheet("CharaMakeType", language).await?;
    let customize = backend
        .excel()
        .get_sheet("CharaMakeCustomize", language)
        .await?;

    // Set number and icon of every choice the creator offers, whatever menu holds it.
    let mut offered = BTreeMap::new();
    for id in customize.get_row_ids() {
        let Ok(row) = customize.get_row(id) else {
            continue;
        };
        let (Ok(icon), Ok(feature)) = (row.read::<u32>(0), row.read::<u8>(14)) else {
            continue;
        };
        offered.insert(id, (u16::from(feature), icon));
    }

    let mut best = Icons::default();
    let mut covered = 0;
    for id in types.get_row_ids() {
        let Ok(row) = types.get_row(id) else {
            continue;
        };
        let faces = params(&row, FACE);
        let hairs = params(&row, HAIR);
        let held: BTreeMap<u16, u32> = hairs
            .iter()
            .filter_map(|param| offered.get(&(*param as u32)).copied())
            .collect();
        let shared = held.keys().filter(|set| on_disk.contains(set)).count();
        if shared <= covered {
            continue;
        }
        covered = shared;
        best = Icons {
            // The face menu numbers its choices by where they sit in it.
            faces: faces
                .iter()
                .enumerate()
                .map(|(at, icon)| (at as u16 + 1, *icon as u32))
                .collect(),
            hairs: held,
        };
    }
    Ok(best)
}

/// The choices the menu driving one customisation offers, as the row states them.
fn params(row: &ExcelRow<'_>, customize: i32) -> Vec<i32> {
    for menu in 0..MENUS {
        let at = menu * STRIDE;
        if row.read::<i32>(at + CUSTOMIZE).ok() != Some(customize) {
            continue;
        }
        return (0..PARAM_COUNT)
            .filter_map(|param| row.read::<i32>(at + PARAMS + param * 4).ok())
            .take_while(|param| *param != 0)
            .collect();
    }
    Vec::new()
}
