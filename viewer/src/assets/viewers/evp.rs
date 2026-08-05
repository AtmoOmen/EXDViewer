//! `.evp` equipment VFX parameters: the per-set flags consulted for the sets whose `.eqp` entry
//! says to use them.
//!
//! Each set carries 512 flag bytes. Bit 0 applies to the body and bit 1 to the head; what the
//! position within the array selects is not identified, so a set is shown as the values its
//! positions take and how many hold each.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::evp::{EquipmentVfxParameter, FLAG_COUNT};

use super::{Preview, facts, line, section, table};

const COLUMNS: [(&str, usize); 2] = [("Set", 6), ("Flags", 8)];

/// The flags a byte sets, in bit order.
fn flags(byte: u8) -> String {
    let listed = [(byte & 0x01 != 0, "body"), (byte & 0x02 != 0, "head")]
        .iter()
        .filter(|(set, _)| *set)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join(" and ");
    match listed.is_empty() {
        true => "nothing".to_owned(),
        false => listed,
    }
}

/// VFX parameters, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    rows: Vec<(u16, String)>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = EquipmentVfxParameter::read(Cursor::new(bytes.to_vec()))?;

    let rows = file
        .sets()
        .iter()
        .filter_map(|set| {
            let flags = file.flags(*set)?;
            let mut values: Vec<(u8, usize)> = Vec::new();
            for byte in flags {
                match values.iter_mut().find(|(value, _)| value == byte) {
                    Some((_, count)) => *count += 1,
                    None => values.push((*byte, 1)),
                }
            }
            let summary = values
                .iter()
                .map(|(value, count)| format!("{count} {} ({value:#04x})", self::flags(*value)))
                .collect::<Vec<_>>()
                .join(", ");
            Some((*set, summary))
        })
        .collect::<Vec<_>>();

    let identity = vec![
        ("Sets", rows.len().to_string()),
        ("Flags per set", FLAG_COUNT.to_string()),
    ];

    log::info!("assets/evp: {path} {} sets", rows.len());

    Ok(Preview::Evp(Box::new(Rendered { identity, rows })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    section(ui, "Sets");
    table(ui, &COLUMNS, file.rows.len(), |ui, index| {
        let (set, summary) = &file.rows[index];
        let cells = [set.to_string(), summary.clone()];
        ui.label(RichText::new(line(&COLUMNS, cells.iter().map(String::as_str))).monospace());
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "evp_identity", &self.identity));
    }
}
