//! The letterboxed dialogue box the game shows over a scene, and the question prompt beside it.
//!
//! The dialogue box's geometry and font sizes come from `ui/uld/BattleTalk.uld`, measured node by
//! node: a ribbon panel behind a name tag, name text at size 14 left-aligned, body text at size 18
//! top-left of the ribbon. It is not drawn pixel-for-pixel: the ribbon is a textured nine-grid the
//! game tiles from an atlas, replaced here with a flat rounded panel, and the fill/edge colors
//! approximate the cream and near-black swatches measured elsewhere in this project rather than
//! the UI theme's own palette, which no shipped sheet names.
//!
//! The question prompt shares that panel but not `ui/uld/SelectStringCutScene.uld`'s own layout
//! (a header pill over a list of fixed-height rows): its rows are the same `Button::selectable`
//! list the flat step list already draws, just boxed to match.

use egui::{Button, Color32, CornerRadius, Margin, RichText, Ui};

const PANEL: Color32 = Color32::from_rgba_premultiplied(18, 14, 10, 210);
const TAG: Color32 = Color32::from_rgba_premultiplied(42, 33, 20, 235);
const FILL: Color32 = Color32::from_rgb(0xeb, 0xe3, 0xc5);

/// A speaker's name over their line, boxed the way `BattleTalk.uld` frames the two.
pub fn ui(ui: &mut Ui, speaker: &str, text: RichText) {
    egui::Frame::new()
        .fill(PANEL)
        .corner_radius(CornerRadius::from(8))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            if !speaker.is_empty() {
                egui::Frame::new()
                    .fill(TAG)
                    .corner_radius(CornerRadius::from(4))
                    .inner_margin(Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.label(RichText::new(speaker).color(FILL).size(14.0).strong());
                    });
                ui.add_space(6.0);
            }
            ui.label(text.color(FILL).size(18.0));
        });
}

/// A branch's arms as a question prompt, one selectable row per label. Returns the row clicked.
pub fn options_ui(ui: &mut Ui, labels: &[String], taken: usize) -> Option<usize> {
    let mut picked = None;
    egui::Frame::new()
        .fill(PANEL)
        .corner_radius(CornerRadius::from(8))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            for (at, label) in labels.iter().enumerate() {
                if ui.add(Button::selectable(at == taken, label)).clicked() {
                    picked = Some(at);
                }
            }
        });
    picked
}
