//! Dressing a character out of the files its body, face and hair are filed under.
//!
//! A code names a body, and everything that body can wear sits under it: `obj/body`, `obj/face` and
//! `obj/hair` each hold a numbered set, and a set's `model` directory holds every piece of it. What
//! a set is made of is read from that directory rather than from a list of suffixes, since a face is
//! several models and which ones it carries is the file tree's to say.
//!
//! What each set is offered under comes from the creator's own menus, in [`menus`].

mod menus;

use std::collections::BTreeSet;
use std::rc::Rc;

use anyhow::Result;
use egui::{CentralPanel, Color32, RichText, ScrollArea, containers::panel::Panel};
use ironworks::excel::Language;

use crate::assets::viewers::mdl;
use crate::backend::Backend;
use crate::data::get_icon_path;
use crate::data::listing::{Listed, Listing};
use crate::excel::provider::ExcelProvider;
use crate::settings::{LANGUAGE, api_base};
use crate::utils::{CollapsibleSidePanel, IconManager, ManagedIcon, Side, TrackedPromise};

/// The bodies a code's first pair can name, and the variants its second can. Every pairing is
/// offered only where the listing holds a body model for it.
const BODIES: std::ops::RangeInclusive<u16> = 1..=18;
const VARIANTS: [u16; 2] = [1, 4];

/// How big a set's icon is drawn.
const ICON: f32 = 40.0;

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
    codes: Vec<u16>,
    /// What the creator offers, and which of its races, clans and genders is being built.
    creator: menus::Creator,
    reading: Option<TrackedPromise<Result<menus::Creator>>>,
    race: u32,
    tribe: u32,
    female: bool,
    /// The code the picked clan and gender resolve to, and the sets it carries.
    code: u16,
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
            codes: Vec::new(),
            creator: menus::Creator::default(),
            reading: None,
            race: 1,
            tribe: 1,
            female: false,
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
        self.codes.clear();
        self.creator = menus::Creator::default();
        self.reading = None;
        self.faces.clear();
        self.hairs.clear();
        self.worn.clear();
        self.fetching = None;
        self.model = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, backend: &Backend, icons: &IconManager) {
        self.poll(ui.ctx(), backend);
        self.side_panel(ui, backend, icons);
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
        if self.codes.is_empty() {
            self.codes = BODIES
                .flat_map(|body| VARIANTS.map(|variant| body * 100 + variant))
                .filter(|code| !body(&listing, *code).is_empty())
                .collect();
            let backend = backend.clone();
            let language = LANGUAGE.get(ctx);
            self.reading = Some(TrackedPromise::spawn_local(async move {
                menus::read(&backend, language).await
            }));
        }
        if matches!(&self.reading, Some(promise) if promise.try_get().is_some())
            && let Some(promise) = self.reading.take()
            && let Some(Ok(read)) = promise.try_get()
        {
            self.creator = menus::Creator {
                bodies: read.bodies.clone(),
                races: read.races.clone(),
                tribes: read.tribes.clone(),
            };
            self.faces.clear();
        }

        if self.faces.is_empty() {
            self.code = resolve(
                &listing,
                &self.codes,
                &self.creator,
                self.tribe,
                self.female,
            );
            self.faces = sets(&listing, &self.code, "face");
            self.hairs = sets(&listing, &self.code, "hair");
            self.face = pick(&self.faces, self.face);
            self.hair = pick(&self.hairs, self.hair);
        }

        let mut wanted = body(&listing, self.code);
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

    fn side_panel(&mut self, ui: &mut egui::Ui, backend: &Backend, icons: &IconManager) {
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
                    let held = self.creator.body(self.tribe, self.female);
                    ui.label(RichText::new("Race").strong());
                    for race in self.creator.races.keys() {
                        if !self.creator.bodies.iter().any(|body| body.race == *race) {
                            continue;
                        }
                        let name = menus::Creator::named(&self.creator.races, *race, self.female);
                        if ui.selectable_label(self.race == *race, name).clicked() {
                            picked = Some(Pick::Race(*race));
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Clan").strong());
                    for body in &self.creator.bodies {
                        if body.race != self.race || body.female != self.female {
                            continue;
                        }
                        let name =
                            menus::Creator::named(&self.creator.tribes, body.tribe, self.female);
                        if ui
                            .selectable_label(self.tribe == body.tribe, name)
                            .clicked()
                        {
                            picked = Some(Pick::Tribe(body.tribe));
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        for (female, name) in [(false, "Male"), (true, "Female")] {
                            if ui.selectable_label(self.female == female, name).clicked() {
                                picked = Some(Pick::Gender(female));
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Face").strong());
                    ui.horizontal_wrapped(|ui| {
                        for set in &self.faces {
                            let icon = held.and_then(|body| body.faces.get(&set.id));
                            if chip(ui, backend, icons, set.id, self.face, icon) {
                                picked = Some(Pick::Face(set.id));
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Hair").strong());
                    ui.horizontal_wrapped(|ui| {
                        for set in &self.hairs {
                            let icon = held.and_then(|body| body.hairs.get(&set.id));
                            if chip(ui, backend, icons, set.id, self.hair, icon) {
                                picked = Some(Pick::Hair(set.id));
                            }
                        }
                    });
                });
                picked
            })
            .and_then(|panel| panel.inner);
        // A body's faces and hair are its own, so clearing them is what reads them again.
        match picked {
            Some(Pick::Race(race)) => {
                self.race = race;
                self.tribe = self
                    .creator
                    .bodies
                    .iter()
                    .find(|body| body.race == race)
                    .map_or(self.tribe, |body| body.tribe);
                self.faces.clear();
            }
            Some(Pick::Tribe(tribe)) => {
                self.tribe = tribe;
                self.faces.clear();
            }
            Some(Pick::Gender(female)) => {
                self.female = female;
                self.faces.clear();
            }
            Some(Pick::Face(face)) => self.face = face,
            Some(Pick::Hair(hair)) => self.hair = hair,
            None => {}
        }
    }
}

enum Pick {
    Race(u32),
    Tribe(u32),
    Gender(bool),
    Face(u16),
    Hair(u16),
}

/// The model code a clan and gender are built on. A code does not name a clan, and two clans share
/// one, so it is found by which of them ships the hair the creator offers for that clan rather than
/// by a table pairing them up.
fn resolve(
    listing: &Listing,
    codes: &[u16],
    creator: &menus::Creator,
    tribe: u32,
    female: bool,
) -> u16 {
    let Some(body) = creator.body(tribe, female) else {
        return *codes.first().unwrap_or(&101);
    };
    codes
        .iter()
        .max_by_key(|code| {
            sets(listing, code, "hair")
                .iter()
                .filter(|set| body.hairs.contains_key(&set.id))
                .count()
        })
        .copied()
        .unwrap_or(101)
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

/// The body a code is built on, which is the lowest set it ships: only one code carries `b0001`,
/// and the rest start wherever their own files do.
fn body(listing: &Listing, code: u16) -> Vec<String> {
    sets(listing, &code, "body")
        .first()
        .map(|set| set.parts.clone())
        .unwrap_or_default()
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

/// One set to pick from: the icon the creator offers it under where there is one, and its number
/// where there is not, since a set the menus do not list still has a model on disk.
fn chip(
    ui: &mut egui::Ui,
    backend: &Backend,
    icons: &IconManager,
    id: u16,
    current: u16,
    icon: Option<&u32>,
) -> bool {
    let Some(icon) = icon else {
        return ui.selectable_label(current == id, id.to_string()).clicked();
    };
    let path = get_icon_path(backend.icons(), *icon, false, Language::None);
    let excel = backend.excel().clone();
    let held = icons.get_or_insert_icon(&path, ui.ctx(), || {
        let path = path.clone();
        TrackedPromise::spawn_local(async move { excel.get_icon(&path).await })
    });
    match held {
        ManagedIcon::Loaded(source) => ui
            .add(
                egui::Button::image(
                    egui::Image::new(source)
                        .maintain_aspect_ratio(true)
                        .fit_to_exact_size(egui::Vec2::splat(ICON)),
                )
                .selected(current == id),
            )
            .on_hover_text(id.to_string())
            .clicked(),
        _ => ui.selectable_label(current == id, id.to_string()).clicked(),
    }
}
