//! `.pap` animation packs: the motions one skeleton can play, and the timeline driving each.
//!
//! The motions themselves are a Havok container, which is left as the bytes it occupies; everything
//! else the file holds is the animation table and one `.tmb` timeline per animation.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::pap::{AnimationPack, ModelType};
use ironworks::file::{File, tmb};

use super::{Preview, chara, facts, heading, section, tmb as timeline};
use crate::assets::Bytes;

/// One animation of the pack, and the timeline it plays alongside.
struct Animation {
    name: String,
    kind: u16,
    havok_index: i16,
    face: bool,
    items: timeline::Items,
}

/// An animation pack, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    animations: Vec<Animation>,
}

/// The model these animations are built for, written the way its own files are named.
fn model(kind: ModelType, id: u16) -> String {
    match kind {
        ModelType::Human => chara::described(id),
        ModelType::Monster => format!("m{id:04}"),
        ModelType::DemiHuman => format!("d{id:04}"),
        ModelType::Weapon => format!("w{id:04}"),
        ModelType::Unknown(_) => id.to_string(),
    }
}

fn model_type(kind: ModelType) -> String {
    match kind {
        ModelType::Unknown(value) => format!("Unknown ({value})"),
        named => format!("{named:?}"),
    }
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = AnimationPack::read(Cursor::new(bytes.to_vec()))?;

    let animations = file
        .animations()
        .iter()
        .zip(file.timelines())
        .map(|(animation, bytes)| {
            Ok(Animation {
                name: animation.name().to_owned(),
                kind: animation.animation_type(),
                havok_index: animation.havok_index(),
                face: animation.face(),
                items: timeline::Items::new(tmb::Timeline::read(Cursor::new(bytes.clone()))?),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let identity = vec![
        ("Version", format!("{:#010x}", file.version())),
        ("Model", model(file.model_type(), file.model_id())),
        ("Model type", model_type(file.model_type())),
        ("Variant", file.variant().to_string()),
        ("Animations", animations.len().to_string()),
        ("Havok", Bytes(file.havok().len()).to_string()),
    ];

    log::info!(
        "assets/pap: {path} {} animations, {} bytes of havok",
        animations.len(),
        file.havok().len()
    );

    Ok(Preview::Pap(Box::new(Rendered {
        identity,
        animations,
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) -> Option<String> {
    let mut follow = None;
    section(ui, "Animations");
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        for animation in &file.animations {
            heading(ui, &animation.name);
            ui.label(
                RichText::new(format!(
                    "type {}, havok motion {}, {}, {} items",
                    animation.kind,
                    animation.havok_index,
                    match animation.face {
                        true => "face",
                        false => "body",
                    },
                    animation.items.count()
                ))
                .monospace()
                .weak(),
            );
            if let Some(path) = animation.items.ui(ui) {
                follow = Some(path);
            }
        }
    });
    follow
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "pap_identity", &self.identity));
    }
}
