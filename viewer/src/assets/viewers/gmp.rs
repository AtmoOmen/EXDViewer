//! `.gmp` gimmick parameters: how a headpiece's visor moves when it is toggled.
//!
//! The file shares its sparse container with `.eqp` and carries no count either, so the sets it
//! holds are found by asking it for every id there could be. A set no block covers reads as zero.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::gmp::{GimmickParameter, Set};

use super::{Preview, facts, line, section, table};

const COLUMNS: [(&str, usize); 5] = [
    ("套装", 6),
    ("面罩", 8),
    ("旋转", 20),
    ("未知 A", 9),
    ("未知 B", 9),
];

/// Whether the file carries this set, which an omitted block reading as zero decides.
fn carried(set: &Set) -> bool {
    set.enabled()
        || set.animated()
        || set.rotation() != [0; 3]
        || set.unknown_a() != 0
        || set.unknown_b() != 0
}

struct Row {
    set: u16,
    visor: &'static str,
    rotation: [u16; 3],
    unknown_a: u8,
    unknown_b: u8,
}

/// Gimmick parameters, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    rows: Vec<Row>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = GimmickParameter::read(Cursor::new(bytes.to_vec()))?;

    let mut rows = Vec::new();
    // Set 0 is the control word rather than a set, and the game reads set 1 in its place.
    for id in 1..=u16::MAX {
        let set = file.set(id);
        if !carried(&set) {
            continue;
        }
        rows.push(Row {
            set: id,
            visor: match (set.enabled(), set.animated()) {
                (true, true) => "动画",
                (true, false) => "开",
                (false, _) => "关",
            },
            rotation: set.rotation(),
            unknown_a: set.unknown_a(),
            unknown_b: set.unknown_b(),
        });
    }

    let identity = vec![
        ("套装数", rows.len().to_string()),
        (
            "动画数",
            rows.iter()
                .filter(|row| row.visor == "动画")
                .count()
                .to_string(),
        ),
    ];

    log::info!("assets/gmp: {path} {} 个套装", rows.len());

    Ok(Preview::Gmp(Box::new(Rendered { identity, rows })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    section(ui, "套装");
    table(ui, &COLUMNS, file.rows.len(), |ui, index| {
        let row = &file.rows[index];
        let cells = [
            row.set.to_string(),
            row.visor.to_owned(),
            format!(
                "{}, {}, {}",
                row.rotation[0], row.rotation[1], row.rotation[2]
            ),
            row.unknown_a.to_string(),
            row.unknown_b.to_string(),
        ];
        ui.label(RichText::new(line(&COLUMNS, cells.iter().map(String::as_str))).monospace());
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "gmp_identity", &self.identity));
    }
}
