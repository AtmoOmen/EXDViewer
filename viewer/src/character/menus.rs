//! What the character creator itself offers, out of `CharaMakeType`, `CharaMakeCustomize` and the
//! `Race` and `Tribe` sheets.
//!
//! A `CharaMakeType` row is one race, clan and gender, holding a menu per customisation that names
//! which it drives in `Customize`. The two menus read here state their choices differently: the face
//! menu holds icon ids outright, and the hair menu holds `CharaMakeCustomize` rows, which carry the
//! set number the file tree uses alongside the icon.
//!
//! Every offset is measured from the file rather than taken from EXDSchema, which is stale here: a
//! menu is 452 bytes, so `SubMenuParam` holds 90 and not the 100 the schema states.

use std::collections::BTreeMap;

use anyhow::Result;
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelRow, ExcelSheet};

/// Menus a row holds, and the bytes one menu is.
const MENUS: u32 = 28;
const STRIDE: u32 = 452;
/// `Customize` and the first `SubMenuParam`, as byte offsets into a menu.
const CUSTOMIZE: u32 = 8;
const PARAMS: u32 = 12;
const PARAM_COUNT: u32 = 90;
/// Where a row names the race, clan and gender it is for.
const RACE: u32 = 13064;
const TRIBE: u32 = 13068;
const GENDER: u32 = 13072;

/// Which customisation a menu drives, as the creator's own numbering has it.
const FACE: i32 = 5;
const HAIR: i32 = 6;

/// `Masculine` and `Feminine`, which is how both sheets name a race and a clan.
const MASCULINE: u32 = 0;
const FEMININE: u32 = 4;

/// One race, clan and gender the creator offers, and what it offers for them.
#[derive(Clone)]
pub struct Body {
    pub race: u32,
    pub tribe: u32,
    pub female: bool,
    /// The icon each face is offered under, by face number.
    pub faces: BTreeMap<u16, u32>,
    /// The icon each hair set is offered under, by the set number the file tree uses.
    pub hairs: BTreeMap<u16, u32>,
}

#[derive(Default)]
pub struct Creator {
    pub bodies: Vec<Body>,
    /// What each race and clan is called, masculine then feminine.
    pub races: BTreeMap<u32, (String, String)>,
    pub tribes: BTreeMap<u32, (String, String)>,
}

impl Creator {
    /// What to call a race or clan, in the gender that is being built.
    pub fn named(named: &BTreeMap<u32, (String, String)>, id: u32, female: bool) -> String {
        match named.get(&id) {
            Some((male, girl)) => match female {
                true => girl.clone(),
                false => male.clone(),
            },
            None => id.to_string(),
        }
    }

    pub fn body(&self, tribe: u32, female: bool) -> Option<&Body> {
        self.bodies
            .iter()
            .find(|body| body.tribe == tribe && body.female == female)
    }
}

pub async fn read(backend: &Backend, language: Language) -> Result<Creator> {
    let excel = backend.excel();
    let types = excel.get_sheet("CharaMakeType", language).await?;
    let customize = excel.get_sheet("CharaMakeCustomize", language).await?;

    // Set number and icon of every choice the creator offers, whatever menu holds it.
    let mut offered = BTreeMap::new();
    for id in customize.get_row_ids() {
        let Ok(row) = customize.get_row(id) else {
            continue;
        };
        if let (Ok(icon), Ok(feature)) = (row.read::<u32>(0), row.read::<u8>(14)) {
            offered.insert(id, (u16::from(feature), icon));
        }
    }

    let mut bodies = Vec::new();
    for id in types.get_row_ids() {
        let Ok(row) = types.get_row(id) else {
            continue;
        };
        let (Ok(race), Ok(tribe), Ok(gender)) = (
            row.read::<i32>(RACE),
            row.read::<i32>(TRIBE),
            row.read::<i8>(GENDER),
        ) else {
            continue;
        };
        if race <= 0 || tribe <= 0 {
            continue;
        }
        bodies.push(Body {
            race: race as u32,
            tribe: tribe as u32,
            female: gender != 0,
            faces: params(&row, FACE)
                .iter()
                .enumerate()
                .map(|(at, icon)| (at as u16 + 1, *icon as u32))
                .collect(),
            hairs: params(&row, HAIR)
                .iter()
                .filter_map(|param| offered.get(&(*param as u32)).copied())
                .collect(),
        });
    }

    Ok(Creator {
        bodies,
        races: names(backend, "Race", language).await?,
        tribes: names(backend, "Tribe", language).await?,
    })
}

/// A sheet's masculine and feminine names, by row.
async fn names(
    backend: &Backend,
    sheet: &str,
    language: Language,
) -> Result<BTreeMap<u32, (String, String)>> {
    let sheet = backend.excel().get_sheet(sheet, language).await?;
    let mut named = BTreeMap::new();
    for id in sheet.get_row_ids() {
        let Ok(row) = sheet.get_row(id) else {
            continue;
        };
        if let (Ok(male), Ok(female)) = (row.read_string(MASCULINE), row.read_string(FEMININE)) {
            let (male, female) = (male.to_string(), female.to_string());
            if !male.is_empty() {
                named.insert(id, (male, female));
            }
        }
    }
    Ok(named)
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
