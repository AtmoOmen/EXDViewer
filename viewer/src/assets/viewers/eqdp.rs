//! `.eqdp` deformer parameters: whether a set has a model and a material of its own for one body.
//!
//! Equipment and accessories share the five slot positions and the file says nothing about which of
//! the two it holds, so the directory it sits in names them.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::eqdp::{EquipmentDeformerParameter, Set};

use super::{Preview, chara, facts, line, section, table};
use crate::utils::file_name;

const EQUIPMENT: [(&str, usize); 6] = [
    ("Set", 6),
    ("Head", 9),
    ("Body", 9),
    ("Hands", 9),
    ("Legs", 9),
    ("Feet", 9),
];

const ACCESSORY: [(&str, usize); 6] = [
    ("Set", 6),
    ("Ears", 9),
    ("Neck", 9),
    ("Wrists", 9),
    ("Ring R", 9),
    ("Ring L", 9),
];

/// What a slot carries of its own, in the order the two bits sit in.
fn carried(material: bool, model: bool) -> &'static str {
    match (material, model) {
        (true, true) => "both",
        (true, false) => "material",
        (false, true) => "model",
        (false, false) => "-",
    }
}

/// The five slots of a set, which the file positions rather than names.
fn slots(set: &Set) -> [&'static str; 5] {
    [
        carried(set.head().material(), set.head().model()),
        carried(set.body().material(), set.body().model()),
        carried(set.hands().material(), set.hands().model()),
        carried(set.legs().material(), set.legs().model()),
        carried(set.feet().material(), set.feet().model()),
    ]
}

/// Deformer parameters, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    columns: &'static [(&'static str, usize)],
    rows: Vec<(u16, [&'static str; 5])>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = EquipmentDeformerParameter::read(Cursor::new(bytes.to_vec()))?;

    let mut rows = Vec::new();
    for id in 0..=u16::MAX {
        let slots = slots(&file.set(id));
        if slots.iter().any(|slot| *slot != "-") {
            rows.push((id, slots));
        }
    }

    let accessory = path.to_lowercase().contains("accessorydeformerparameter");
    let name = file_name(path);
    let mut identity = Vec::new();
    if let Some(code) = name
        .strip_prefix('c')
        .and_then(|code| code.split('.').next())
        .and_then(|code| code.parse().ok())
    {
        identity.push(("Body", chara::described(code)));
    }
    identity.push((
        "Slots",
        match accessory {
            true => "accessory".to_owned(),
            false => "equipment".to_owned(),
        },
    ));
    identity.push(("Sets", rows.len().to_string()));

    log::info!("assets/eqdp: {path} {} sets", rows.len());

    Ok(Preview::Eqdp(Box::new(Rendered {
        identity,
        columns: match accessory {
            true => &ACCESSORY,
            false => &EQUIPMENT,
        },
        rows,
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    section(ui, "Sets");
    table(ui, file.columns, file.rows.len(), |ui, index| {
        let (set, slots) = &file.rows[index];
        let set = set.to_string();
        let cells = std::iter::once(set.as_str()).chain(slots.iter().copied());
        ui.label(RichText::new(line(file.columns, cells)).monospace());
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "eqdp_identity", &self.identity));
    }
}
