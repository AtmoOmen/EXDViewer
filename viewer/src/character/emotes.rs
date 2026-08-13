//! The emotes the game names, out of `Emote` and `ActionTimeline`.
//!
//! An emote names up to seven timelines, of which the first is the motion itself and the rest are
//! the loop it settles into. A timeline's key is the tail of a pack path under the body's own
//! animation directory, which is what turns thousands of numbered files into a named, iconned list.

use anyhow::Result;
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelRow, ExcelSheet};

/// `Emote`'s name, icon and the timelines it plays, and `ActionTimeline`'s key, as byte offsets.
const NAME: u32 = 0;
const ICON: u32 = 4;
const TIMELINES: [u32; 7] = [16, 18, 20, 22, 24, 26, 28];
const KEY: u32 = 0;

/// One emote the creator's own list would show.
pub struct Emote {
    pub name: String,
    pub icon: u32,
    /// The pack each of its timelines is filed under, in the order the row states them.
    pub keys: Vec<String>,
}

impl Emote {
    /// Where the pack a body plays this from lives. A key is filed under the body's own code, so a
    /// character of another race reads the same emote out of its own directory.
    pub fn pack(&self, code: u16, at: usize) -> Option<String> {
        let key = self.keys.get(at)?;
        Some(format!(
            "chara/human/c{code:04}/animation/a0001/bt_common/{key}.pap"
        ))
    }
}

/// Every emote the game both names and animates, in name order.
pub async fn read(backend: &Backend, language: Language) -> Result<Vec<Emote>> {
    let excel = backend.excel();
    let emotes = excel.get_sheet("Emote", language).await?;
    let timelines = excel.get_sheet("ActionTimeline", language).await?;

    let mut found = Vec::new();
    for id in emotes.get_row_ids() {
        let Ok(row) = emotes.get_row(id) else {
            continue;
        };
        let (Ok(name), Ok(icon)) = (row.read_string(NAME), row.read::<u32>(ICON)) else {
            continue;
        };
        let name = name.to_string();
        if name.is_empty() || icon == 0 {
            continue;
        }
        let keys: Vec<String> = TIMELINES
            .iter()
            .filter_map(|at| row.read::<u16>(*at).ok())
            .filter(|timeline| *timeline > 0)
            .filter_map(|timeline| key(&timelines, u32::from(timeline)))
            .collect();
        if keys.is_empty() {
            continue;
        }
        found.push(Emote { name, icon, keys });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!("character: {} emotes to play", found.len());
    Ok(found)
}

fn key(timelines: &impl ExcelSheet, id: u32) -> Option<String> {
    let row: ExcelRow<'_> = timelines.get_row(id).ok()?;
    let key = row.read_string(KEY).ok()?.to_string();
    (!key.is_empty()).then_some(key)
}
