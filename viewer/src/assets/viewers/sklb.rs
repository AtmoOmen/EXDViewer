//! `.sklb` skeletons: the layers an animation drives, and the bones a skeleton hangs off its
//! parent by.
//!
//! The skeleton itself is a Havok binary tagfile, which is left as the bytes it occupies.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::sklb::SkeletonBinary;

use super::{Preview, chara, facts, heading, section};
use crate::assets::Bytes;
use crate::utils::file_name;

/// The bones a list names, with the unset slots the header pads it out with dropped.
fn bones(indices: &[i16]) -> String {
    let listed = indices
        .iter()
        .filter(|index| **index >= 0)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match listed.is_empty() {
        true => "none".to_owned(),
        false => listed,
    }
}

/// A skeleton, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    /// Each layer's own flag word, and the bones it drives.
    layers: Vec<(u32, String)>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = SkeletonBinary::read(Cursor::new(bytes.to_vec()))?;

    let layers = file
        .animation_layers()
        .iter()
        .map(|layer| (layer.layer(), bones(layer.bone_indices())))
        .collect::<Vec<_>>();

    let character = file.character_id();
    let mut identity = vec![
        ("Version", format!("{:?}", file.version())),
        (
            "Character",
            match u16::try_from(character) {
                // Only the human skeletons are filed under a character code the code names.
                Ok(code) if file_name(path).starts_with("skl_c") => chara::described(code),
                _ => character.to_string(),
            },
        ),
        ("Connect bones", bones(&file.connect_bones())),
    ];
    if let Some(counts) = file.lod_sample_bone_count() {
        identity.push((
            "LOD sample bones",
            counts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    identity.push((
        "Mapper characters",
        file.mapper_character_id()
            .iter()
            .filter(|id| **id != u32::MAX)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    ));
    identity.push(("Animation layers", layers.len().to_string()));
    identity.push(("Skeleton", Bytes(file.skeleton().len()).to_string()));

    log::info!(
        "assets/sklb: {path} {} layers, {} bytes of havok",
        layers.len(),
        file.skeleton().len()
    );

    Ok(Preview::Sklb(Box::new(Rendered { identity, layers })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        section(ui, "Animation layers");
        for (layer, bones) in &file.layers {
            heading(ui, &format!("Layer {layer:#010x}"));
            ui.label(RichText::new(bones).monospace());
        }
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "sklb_identity", &self.identity));
    }
}
