//! `.exl` the sheet list: every Excel sheet the game ships, under the id it is listed by.

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::{File, exl};
use std::io::Cursor;

use super::{Preview, facts, line, link, section, table};

/// The list's two columns, the id padded to the width its cells are drawn at. The sheet is a link
/// rather than a padded cell, so it sits at the end.
const COLUMNS: [(&str, usize); 2] = [("ID", 5), ("表格", 8)];

/// A sheet list, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    /// Sheet name and the id the list gives it, in name order.
    rows: Vec<(String, i32)>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let list = exl::ExcelList::read(Cursor::new(bytes.to_vec()))?;
    let mut rows: Vec<(String, i32)> = list.0.into_iter().collect();
    // The list reads into a map, so an order is put back rather than kept. Case matters to neither
    // the game nor anyone looking a sheet up.
    rows.sort_by_cached_key(|(name, _)| name.to_lowercase());

    log::info!("assets/exl: {path} {} 个表格", rows.len());

    Ok(Preview::Exl(Box::new(Rendered {
        identity: vec![("表格数", rows.len().to_string())],
        rows,
    })))
}

pub fn ui(ui: &mut egui::Ui, list: &Rendered) -> Option<String> {
    let mut follow = None;
    section(ui, "表格");
    table(ui, &COLUMNS, list.rows.len(), |ui, index| {
        let (name, id) = &list.rows[index];
        let id = id.to_string();
        ui.horizontal(|ui| {
            // The link is a widget of its own where the id is a padded string, so the spacing
            // between them has to go for it to land under its header.
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new(line(&COLUMNS, [id.as_str()])).monospace());
            let path = format!("exd/{}.exh", name.to_lowercase());
            if link(ui, name, &path) {
                follow = Some(path);
            }
        });
    });
    follow
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "exl_identity", &self.identity));
    }
}
