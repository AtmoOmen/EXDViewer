//! The dyes the game names, out of `Stain`.
//!
//! A row's own id is the stain a `.stm` template reads a color by, so the sheet exists only to
//! name and swatch what a template already carries a value for.

use anyhow::Result;
use egui::Color32;
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelSheet};

/// `Stain`'s name and color, as byte offsets. The color's leading byte is unused; the other three
/// are red, green and blue in file order, and the sheet states no alpha.
const NAME: u32 = 4;
const COLOR: u32 = 8;

/// One dye the picker offers.
pub struct Stain {
    pub id: u8,
    pub name: String,
    pub color: Color32,
}

/// Every stain the game names, in row order. Row 0 is the unstained slot and is never returned.
pub async fn read(backend: &Backend, language: Language) -> Result<Vec<Stain>> {
    let excel = backend.excel();
    let sheet = excel.get_sheet("Stain", language).await?;
    let mut found = Vec::new();
    for id in sheet.get_row_ids() {
        let Ok(id) = u8::try_from(id) else { continue };
        if id == 0 {
            continue;
        }
        let Ok(row) = sheet.get_row(u32::from(id)) else {
            continue;
        };
        let Ok(name) = row.read_string(NAME) else {
            continue;
        };
        let name = name.to_string();
        if name.is_empty() {
            continue;
        }
        let color: u32 = row.read(COLOR).unwrap_or(0);
        let [_, r, g, b] = color.to_be_bytes();
        found.push(Stain {
            id,
            name,
            color: Color32::from_rgb(r, g, b),
        });
    }
    log::info!("character: {} dyes to pick from", found.len());
    Ok(found)
}
