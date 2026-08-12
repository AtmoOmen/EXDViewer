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

use super::{Gear, Outfit, Slot};
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

/// Where `Race` names the item worn in each of [`Slot::RACIAL`], masculine then feminine.
const RSE: u32 = 8;
/// `Item`'s model quad.
const MODEL: u32 = 24;
/// `CharaMakeClassEquip`'s class, after the seven quads it dresses that class in.
const CLASS_JOB: u32 = 56;
/// `ClassJob`'s name as the creator writes it, rather than the lowercase one it is filed under.
const JOB_NAME: u32 = 16;

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

/// One of the classes the creator starts a character in, and what it dresses them in.
pub struct Job {
    pub name: String,
    pub outfit: Outfit,
}

#[derive(Default)]
pub struct Creator {
    pub bodies: Vec<Body>,
    /// What each race and clan is called, masculine then feminine.
    pub races: BTreeMap<u32, (String, String)>,
    pub tribes: BTreeMap<u32, (String, String)>,
    /// The clothing a race wears when it wears nothing else, by race and gender.
    pub attire: BTreeMap<(u32, bool), Outfit>,
    pub jobs: Vec<Job>,
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
                .filter_map(|icon| Some((face(*icon)?, *icon as u32)))
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
        attire: attire(backend, language).await?,
        jobs: jobs(backend, language).await?,
    })
}

/// What each race stands in when it is wearing nothing else. `Race` names an item per slot and
/// gender, and the item's model quad is the set and the variant it is worn at.
async fn attire(backend: &Backend, language: Language) -> Result<BTreeMap<(u32, bool), Outfit>> {
    let excel = backend.excel();
    let races = excel.get_sheet("Race", language).await?;
    let items = excel.get_sheet("Item", language).await?;
    let mut dressed = BTreeMap::new();
    for id in races.get_row_ids() {
        let Ok(race) = races.get_row(id) else {
            continue;
        };
        for female in [false, true] {
            let mut outfit = Outfit::default();
            for (at, slot) in Slot::RACIAL.into_iter().enumerate() {
                let at = RSE + at as u32 * 8 + u32::from(female) * 4;
                outfit[slot as usize] = race
                    .read::<i32>(at)
                    .ok()
                    .filter(|item| *item > 0)
                    .and_then(|item| items.get_row(item as u32).ok())
                    .and_then(|item| item.read::<u64>(MODEL).ok())
                    .and_then(Gear::read);
            }
            dressed.insert((id, female), outfit);
        }
    }
    Ok(dressed)
}

/// The classes a character can be started as, and the gear each of them is started in. The sheet
/// states its five armour quads in [`Slot::ALL`]'s own order, ahead of the two it holds.
async fn jobs(backend: &Backend, language: Language) -> Result<Vec<Job>> {
    let excel = backend.excel();
    let equipped = excel.get_sheet("CharaMakeClassEquip", language).await?;
    let classes = excel.get_sheet("ClassJob", language).await?;
    let mut found = Vec::new();
    for id in equipped.get_row_ids() {
        let Ok(row) = equipped.get_row(id) else {
            continue;
        };
        let Ok(class) = row.read::<i32>(CLASS_JOB) else {
            continue;
        };
        let mut outfit = Outfit::default();
        for (at, held) in outfit.iter_mut().enumerate() {
            *held = row.read::<u64>(at as u32 * 8).ok().and_then(Gear::read);
        }
        found.push(Job {
            name: classes
                .get_row(class as u32)
                .ok()
                .and_then(|row| row.read_string(JOB_NAME).ok().map(|name| name.to_string()))
                .unwrap_or_else(|| class.to_string()),
            outfit,
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(found)
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

/// Which face an icon is offered for, which is the last two digits of the icon's own number rather
/// than where it sits in the menu. Hrothgar are what tells the two apart: both of theirs offer four
/// faces numbered 5 to 8, so reading them off their positions would draw four other faces entirely,
/// and one of the two codes ships no lower face at all.
fn face(icon: i32) -> Option<u16> {
    match icon % 100 {
        0 => None,
        id => Some(id as u16),
    }
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
