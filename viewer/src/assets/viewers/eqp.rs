//! `.eqp` equipment parameters: what a piece of equipment hides and reveals on the character
//! wearing it.
//!
//! The file is a sparse table with no count of its own, so the sets it carries are found by asking
//! it for every id there could be. A set no block covers reads as the game's default, which enables
//! nothing, and that is what tells the two apart.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::eqp::{EquipmentParameter, Set};

use super::{Preview, facts, line, section, table};

const COLUMNS: [(&str, usize); 3] = [("Set", 6), ("Slot", 6), ("Flags", 8)];

macro_rules! flags {
    ($slot:expr, $($flag:ident)*) => {{
        let slot = $slot;
        [$((slot.$flag(), stringify!($flag)),)*]
            .iter()
            .filter(|(set, _)| *set)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    }};
}

/// The flags a set holds, by slot, dropping the slots it says nothing about.
fn slots(set: &Set) -> Vec<(&'static str, String)> {
    [
        (
            "body",
            flags!(
                set.body(),
                enabled hide_waist hide_thighs hide_gloves_small hide_glove_cuffs
                hide_gloves_medium hide_gloves_large hide_gorget show_legs show_hands show_head
                show_necklace show_bracelets show_tail disable_breast_physics uses_vfx_parameter
            ),
        ),
        (
            "legs",
            flags!(
                set.legs(),
                enabled hide_knee_pads hide_boots_small hide_boots_medium show_feet show_tail
            ),
        ),
        (
            "hands",
            flags!(
                set.hands(),
                enabled hide_elbow hide_forearm over_sleeve show_bracelets show_ring_left
                show_ring_right
            ),
        ),
        (
            "feet",
            flags!(set.feet(), enabled hide_knee hide_calf hide_ankle),
        ),
        (
            "head",
            flags!(
                set.head(),
                enabled hide_scalp hide_hair show_hair_override hide_neck show_necklace
                show_earrings_hyur_roegadyn show_earrings_elezen_lalafell
                show_earrings_miqote_hrothgar_viera show_earrings_au_ra show_ears_human
                show_ears_miqote show_ears_au_ra show_ears_viera disable_bangs_physics
                disable_hair_physics show_on_hrothgar show_on_viera uses_vfx_parameter
            ),
        ),
    ]
    .into_iter()
    .filter(|(_, flags)| !flags.is_empty())
    .collect()
}

/// Whether the file carries this set, which the default entry enabling no slot at all decides.
fn carried(set: &Set) -> bool {
    set.body().enabled()
        || set.legs().enabled()
        || set.hands().enabled()
        || set.feet().enabled()
        || set.head().enabled()
}

/// One slot of one set.
struct Row {
    set: u16,
    slot: &'static str,
    flags: String,
}

/// Equipment parameters, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    rows: Vec<Row>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = EquipmentParameter::read(Cursor::new(bytes.to_vec()))?;

    let mut sets = 0;
    let mut rows = Vec::new();
    // Set 0 is the control word rather than a set, and the game reads set 1 in its place.
    for id in 1..=u16::MAX {
        let set = file.set(id);
        if !carried(&set) {
            continue;
        }
        sets += 1;
        rows.extend(slots(&set).into_iter().map(|(slot, flags)| Row {
            set: id,
            slot,
            flags,
        }));
    }

    let identity = vec![
        ("Sets", sets.to_string()),
        ("Entries", rows.len().to_string()),
    ];

    log::info!("assets/eqp: {path} {sets} sets");

    Ok(Preview::Eqp(Box::new(Rendered { identity, rows })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    section(ui, "套装");
    table(ui, &COLUMNS, file.rows.len(), |ui, index| {
        let row = &file.rows[index];
        let cells = [row.set.to_string(), row.slot.to_owned(), row.flags.clone()];
        ui.label(RichText::new(line(&COLUMNS, cells.iter().map(String::as_str))).monospace());
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "eqp_identity", &self.identity));
    }
}
