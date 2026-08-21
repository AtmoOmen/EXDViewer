//! The mounts the game names, out of `Mount` and `ModelChara`.
//!
//! A mount names its model by way of `ModelChara`, which states the kind of body it is drawn from,
//! the numbered set under it and the variant it is worn at. A monster is one whole model and a
//! demihuman is several pieces of equipment, so what is kept here is the directory they sit in
//! rather than a list of suffixes.

use anyhow::Result;
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelSheet};

/// `Mount`'s name, the model it names and the icon it is offered under, as byte offsets.
const NAME: u32 = 0;
const CHARA: u32 = 28;
const ICON: u32 = 52;
/// `ModelChara`'s numbered body, the kind of body it is, the set under it, and the variant.
const MODEL: u32 = 12;
const KIND: u32 = 16;
const BASE: u32 = 17;
const VARIANT: u32 = 18;

/// The two kinds of body a mount is drawn from.
const DEMIHUMAN: u8 = 2;
const MONSTER: u8 = 3;

/// One mount the game both names and holds a model of.
pub struct Mount {
    pub name: String,
    pub icon: u32,
    /// Where the models it is drawn from sit.
    pub under: String,
    /// Which of the set's variants it is drawn at.
    pub variant: u16,
}

/// Every mount the game names, in name order.
pub async fn read(backend: &Backend, language: Language) -> Result<Vec<Mount>> {
    let excel = backend.excel();
    let mounts = excel.get_sheet("Mount", language).await?;
    let models = excel.get_sheet("ModelChara", language).await?;

    let mut found = Vec::new();
    for id in mounts.get_row_ids() {
        let Ok(row) = mounts.get_row(id) else {
            continue;
        };
        let (Ok(name), Ok(icon), Ok(chara)) = (
            row.read_string(NAME),
            row.read::<u16>(ICON),
            row.read::<u32>(CHARA),
        ) else {
            continue;
        };
        let name = name.to_string();
        if name.is_empty() || chara == 0 {
            continue;
        }
        let Ok(held) = models.get_row(chara) else {
            continue;
        };
        let (Ok(model), Ok(kind), Ok(base), Ok(variant)) = (
            held.read::<u16>(MODEL),
            held.read::<u8>(KIND),
            held.read::<u8>(BASE),
            held.read::<u8>(VARIANT),
        ) else {
            continue;
        };
        let under = match kind {
            MONSTER => format!("chara/monster/m{model:04}/obj/body/b{base:04}/model/"),
            DEMIHUMAN => format!("chara/demihuman/d{model:04}/obj/equipment/e{base:04}/model/"),
            _ => continue,
        };
        found.push(Mount {
            name,
            icon: u32::from(icon),
            under,
            variant: u16::from(variant),
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!("character: {} mounts to stand on", found.len());
    Ok(found)
}
