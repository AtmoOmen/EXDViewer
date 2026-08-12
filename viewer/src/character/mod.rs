//! Dressing a character out of the files its body, face and hair are filed under.
//!
//! A code names a body, and everything that body can wear sits under it: `obj/body`, `obj/face` and
//! `obj/hair` each hold a numbered set, and a set's `model` directory holds every piece of it. What
//! a set is made of is read from that directory rather than from a list of suffixes, since a face is
//! several models and which ones it carries is the file tree's to say.

use std::collections::BTreeSet;
use std::rc::Rc;

use anyhow::Result;
use egui::{CentralPanel, Color32, RichText, ScrollArea, containers::panel::Panel};

use crate::assets::viewers::{chara, mdl};
use crate::backend::Backend;
use crate::data::listing::{Listed, Listing};
use crate::settings::api_base;
use crate::utils::{CollapsibleSidePanel, Side, TrackedPromise};

/// The bodies a code's first pair can name, and the variants its second can. Every pairing is
/// offered only where the listing holds a body model for it.
const BODIES: std::ops::RangeInclusive<u16> = 1..=18;
const VARIANTS: [u16; 2] = [1, 4];

/// The one body set a character is built on. A code carries others, but they are the same shape
/// wearing different equipment, which is [`super::assets`]' job rather than this one's.
const BODY: u16 = 1;

/// The files a character is worn out of, each with its bytes.
type Worn = Vec<(String, Vec<u8>)>;

/// What a picked set is made of, and what to call it.
struct Set {
    id: u16,
    parts: Vec<String>,
}

pub struct CharacterBuilder {
    listing: Option<Rc<Listing>>,
    /// Codes the install ships a body for, in order.
    bodies: Vec<u16>,
    code: u16,
    /// The face and hair sets the picked code carries.
    faces: Vec<Set>,
    hairs: Vec<Set>,
    face: u16,
    hair: u16,
    /// The files the model on screen was built from, so a pick that changes nothing costs nothing.
    worn: Vec<String>,
    fetching: Option<TrackedPromise<Result<Worn>>>,
    model: Option<Result<Box<mdl::Rendered>, String>>,
}

impl Default for CharacterBuilder {
    fn default() -> Self {
        Self {
            listing: None,
            bodies: Vec::new(),
            code: 101,
            faces: Vec::new(),
            hairs: Vec::new(),
            face: 1,
            hair: 1,
            worn: Vec::new(),
            fetching: None,
            model: None,
        }
    }
}

impl CharacterBuilder {
    /// Drop everything that came from the install, so a reconnect reads it all again.
    pub fn reset(&mut self) {
        self.listing = None;
        self.bodies.clear();
        self.faces.clear();
        self.hairs.clear();
        self.worn.clear();
        self.fetching = None;
        self.model = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, backend: &Backend) {
        self.poll(ui.ctx(), backend);
        self.side_panel(ui);
        CentralPanel::default().show(ui, |ui| match &self.model {
            Some(Ok(model)) => mdl::ui(ui, model, backend),
            Some(Err(why)) => {
                ui.centered_and_justified(|ui| {
                    ui.colored_label(Color32::LIGHT_RED, why);
                });
            }
            None => {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            }
        });
    }

    fn poll(&mut self, ctx: &egui::Context, backend: &Backend) {
        if self.listing.is_none() {
            match backend.listing(&api_base(ctx)) {
                Listed::Loading => return,
                Listed::Ready(listing) => self.listing = Some(listing),
                Listed::Failed(why) => {
                    self.model = Some(Err(why.to_string()));
                    return;
                }
            }
        }
        let Some(listing) = self.listing.clone() else {
            return;
        };
        if self.bodies.is_empty() {
            self.bodies = BODIES
                .flat_map(|body| VARIANTS.map(|variant| body * 100 + variant))
                .filter(|code| {
                    !parts(&listing, &format!("{}/obj/body", root(*code)), BODY).is_empty()
                })
                .collect();
            if !self.bodies.contains(&self.code)
                && let Some(first) = self.bodies.first()
            {
                self.code = *first;
            }
        }
        if self.faces.is_empty() {
            self.faces = sets(&listing, &self.code, "face");
            self.hairs = sets(&listing, &self.code, "hair");
            self.face = pick(&self.faces, self.face);
            self.hair = pick(&self.hairs, self.hair);
        }

        let mut wanted = parts(&listing, &format!("{}/obj/body", root(self.code)), BODY);
        wanted.extend(held(&self.faces, self.face));
        wanted.extend(held(&self.hairs, self.hair));
        if wanted != self.worn && !wanted.is_empty() {
            self.worn = wanted.clone();
            let files = backend.files().clone();
            self.fetching = Some(TrackedPromise::spawn_local(async move {
                let mut read = Vec::with_capacity(wanted.len());
                for path in wanted {
                    let bytes = files.read(&path).await?;
                    read.push((path, bytes));
                }
                Ok(read)
            }));
        }
        if matches!(&self.fetching, Some(promise) if promise.try_get().is_some()) {
            let Some(promise) = self.fetching.take() else {
                return;
            };
            self.model = Some(
                promise
                    .try_get()
                    .expect("just landed")
                    .as_ref()
                    .map_err(ToString::to_string)
                    .and_then(|parts| {
                        mdl::compose(parts)
                            .map(Box::new)
                            .map_err(|why| why.to_string())
                    }),
            );
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let picked = CollapsibleSidePanel::new("character_pick", Side::Left)
            .show(ui, |ui, is_open| {
                let mut picked = None;
                if !is_open {
                    return picked;
                }
                Panel::top("character_header").show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        CollapsibleSidePanel::draw_arrow(ui, "character_pick", Side::Left);
                        ui.heading("Character");
                    });
                    ui.add_space(4.0);
                });
                ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new("Body").strong());
                    for code in &self.bodies {
                        let name = chara::name(*code).unwrap_or_else(|| code.to_string());
                        if ui.selectable_label(self.code == *code, name).clicked() {
                            picked = Some(Pick::Code(*code));
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Face").strong());
                    ui.horizontal_wrapped(|ui| {
                        for set in &self.faces {
                            if ui
                                .selectable_label(self.face == set.id, set.id.to_string())
                                .clicked()
                            {
                                picked = Some(Pick::Face(set.id));
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Hair").strong());
                    ui.horizontal_wrapped(|ui| {
                        for set in &self.hairs {
                            if ui
                                .selectable_label(self.hair == set.id, set.id.to_string())
                                .clicked()
                            {
                                picked = Some(Pick::Hair(set.id));
                            }
                        }
                    });
                });
                picked
            })
            .and_then(|panel| panel.inner);
        match picked {
            // A body's faces and hair are its own, so both lists are read again for the new one.
            Some(Pick::Code(code)) => {
                self.code = code;
                self.faces.clear();
                self.hairs.clear();
            }
            Some(Pick::Face(face)) => self.face = face,
            Some(Pick::Hair(hair)) => self.hair = hair,
            None => {}
        }
    }
}

enum Pick {
    Code(u16),
    Face(u16),
    Hair(u16),
}

fn root(code: u16) -> String {
    format!("chara/human/c{code:04}")
}

/// Every model one numbered set of a kind holds. A face is several files and a body one, and which
/// is which is the directory's to say rather than a list of suffixes here.
fn parts(listing: &Listing, under: &str, id: u16) -> Vec<String> {
    let letter = under.rsplit('/').next().unwrap_or_default().as_bytes()[0] as char;
    let mut found = listing.under(&format!("{under}/{letter}{id:04}/model/"));
    found.retain(|path| path.ends_with(".mdl"));
    found.sort();
    found
}

/// The numbered sets of a kind the code carries, each with the models it holds.
fn sets(listing: &Listing, code: &u16, kind: &str) -> Vec<Set> {
    let under = format!("{}/obj/{kind}", root(*code));
    let letter = kind.as_bytes()[0] as char;
    listing
        .under(&format!("{under}/"))
        .iter()
        .filter(|path| path.ends_with(".mdl"))
        .filter_map(|path| {
            let rest = path.strip_prefix(&format!("{under}/{letter}"))?;
            rest.get(..4)?.parse::<u16>().ok()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| Set {
            id,
            parts: parts(listing, &under, id),
        })
        .collect()
}

/// The picked set if the code still carries it, and its lowest otherwise.
fn pick(sets: &[Set], wanted: u16) -> u16 {
    match sets.iter().any(|set| set.id == wanted) {
        true => wanted,
        false => sets.first().map_or(wanted, |set| set.id),
    }
}

fn held(sets: &[Set], wanted: u16) -> Vec<String> {
    sets.iter()
        .find(|set| set.id == wanted)
        .map(|set| set.parts.clone())
        .unwrap_or_default()
}
