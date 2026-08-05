//! `.sklb` skeletons: the bones the file names, the pose they rest in, the layers an animation
//! drives, and the bones a skeleton hangs off its parent by.

use std::cell::{Cell, RefCell};
use std::io::Cursor;

use anyhow::Result;
use egui::{Color32, RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::sklb::SkeletonBinary;

use super::{Preview, chara, facts, heading, placed, section, skeleton::Rig};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Bones,
    Pose,
    Layers,
}

/// A skeleton, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    /// Each layer's own flag word, and the bones it drives.
    layers: Vec<(u32, String)>,
    /// The bones themselves, or why the embedded tagfile gave none.
    rig: std::result::Result<Rig, String>,
    /// The pose on the card, and the bone it was drawn picking out.
    scene: RefCell<Option<(Option<usize>, placed::View)>>,
    picked: Cell<Option<usize>>,
    tab: Cell<Tab>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = SkeletonBinary::read(Cursor::new(bytes.to_vec()))?;

    let layers = file
        .animation_layers()
        .iter()
        .map(|layer| (layer.layer(), bones(layer.bone_indices())))
        .collect::<Vec<_>>();

    let rig = file
        .parse_skeleton()
        .map(|skeleton| {
            Rig::new(
                skeleton.bones(),
                skeleton.parent_indices(),
                skeleton.reference_pose(),
            )
        })
        .map_err(|e| e.to_string());

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
        (
            "Bones",
            match &rig {
                Ok(rig) => rig.bones().to_string(),
                Err(e) => e.clone(),
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

    Ok(Preview::Sklb(Box::new(Rendered {
        identity,
        layers,
        rig,
        scene: RefCell::new(None),
        picked: Cell::new(None),
        tab: Cell::new(Tab::Bones),
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Bones, "Bones"),
            (Tab::Pose, "Reference pose"),
            (Tab::Layers, "Animation layers"),
        ] {
            if ui.selectable_label(file.tab.get() == tab, label).clicked() {
                file.tab.set(tab);
            }
        }
    });
    ui.add_space(4.0);

    let rig = match (file.tab.get(), &file.rig) {
        (Tab::Layers, _) => {
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                for (layer, bones) in &file.layers {
                    heading(ui, &format!("Layer {layer:#010x}"));
                    ui.label(RichText::new(bones).monospace());
                }
            });
            return;
        }
        (_, Err(e)) => {
            ui.centered_and_justified(|ui| {
                ui.colored_label(Color32::RED, format!("Could not read this skeleton: {e}"));
            });
            return;
        }
        (_, Ok(rig)) => rig,
    };

    if file.tab.get() == Tab::Bones {
        section(ui, "Bones");
        rig.tree_ui(ui, rig.reference(), &file.picked);
        return;
    }

    let mut scene = file.scene.borrow_mut();
    let (drawn, view) = scene.get_or_insert_with(|| (None, rig.view(rig.reference())));
    if *drawn != file.picked.get() {
        *drawn = file.picked.get();
        view.replace(rig.batches(&rig.world(rig.reference()), *drawn));
    }
    view.ui(ui);
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "sklb_identity", &self.identity));
    }
}
