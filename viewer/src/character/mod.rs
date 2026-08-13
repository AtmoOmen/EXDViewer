//! Dressing a character out of the files its body, face and hair are filed under.
//!
//! A code names a body, and everything that body can wear sits under it: `obj/body`, `obj/face` and
//! `obj/hair` each hold a numbered set, and a set's `model` directory holds every piece of it. What
//! a set is made of is read from that directory rather than from a list of suffixes, since a face is
//! several models and which ones it carries is the file tree's to say.
//!
//! What each set is offered under comes from the creator's own menus, in [`menus`].

mod menus;

use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use anyhow::Result;
use egui::{CentralPanel, Color32, RichText, ScrollArea, TextEdit, containers::panel::Panel};
use ironworks::excel::Language;

use crate::assets::viewers::mdl;
use crate::backend::Backend;
use crate::data::get_icon_path;
use crate::data::listing::{Listed, Listing};
use crate::excel::provider::ExcelProvider;
use crate::settings::{LANGUAGE, api_base};
use crate::utils::{
    CollapsibleSidePanel, FuzzyMatcher, IconManager, ManagedIcon, Side, TrackedPromise,
};

/// The bodies a code's first pair can name, and the variants its second can. Every pairing is
/// offered only where the listing holds a body model for it.
const BODIES: std::ops::RangeInclusive<u16> = 1..=18;
const VARIANTS: [u16; 2] = [1, 4];

/// How big a set's icon is drawn, and how far apart the grid sets them.
const ICON: f32 = 40.0;
const GAP: f32 = 4.0;

/// How big a piece of equipment's icon is drawn beside its name, and how many of them a slot's
/// picker shows at once.
const PIECE: f32 = 24.0;
const SHOWN: usize = 10;

/// Smallclothes, which is what everything else is worn over.
const SMALLCLOTHES: Gear = Gear { set: 0, variant: 1 };

/// A slot a character wears something in, as the file names abbreviate it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Head,
    Body,
    Hands,
    Legs,
    Feet,
}

impl Slot {
    pub const ALL: [Slot; 5] = [Self::Head, Self::Body, Self::Hands, Self::Legs, Self::Feet];
    /// The slots a race has clothing of its own for, in the order `Race` states them.
    pub const RACIAL: [Slot; 4] = [Self::Body, Self::Hands, Self::Legs, Self::Feet];

    fn name(self) -> &'static str {
        match self {
            Self::Head => "Head",
            Self::Body => "Body",
            Self::Hands => "Hands",
            Self::Legs => "Legs",
            Self::Feet => "Feet",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Head => "met",
            Self::Body => "top",
            Self::Hands => "glv",
            Self::Legs => "dwn",
            Self::Feet => "sho",
        }
    }
}

/// A set and the variant it is worn at, which is how a model quad states a piece of equipment.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Gear {
    pub set: u16,
    pub variant: u16,
}

impl Gear {
    pub fn read(quad: u64) -> Option<Self> {
        (quad != 0).then_some(Self {
            set: quad as u16,
            variant: (quad >> 16) as u16,
        })
    }
}

/// What a character wears, by slot.
pub type Outfit = [Option<Gear>; 5];

/// What the creator dresses a character in before anything is picked for them.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Attire {
    #[default]
    Race,
    Job,
    Smallclothes,
}

/// The bytes of every file read so far, by path, and one batch of them as they land.
type Files = BTreeMap<String, Vec<u8>>;
type Read = Vec<(String, Vec<u8>)>;

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
    reading_pieces: Option<TrackedPromise<Result<[Vec<menus::Piece>; 5]>>>,
    race: u32,
    tribe: u32,
    female: bool,
    /// The code the picked clan and gender resolve to, and the sets it carries.
    code: u16,
    body: Vec<String>,
    faces: Vec<Set>,
    hairs: Vec<Set>,
    face: u16,
    hair: u16,
    attire: Attire,
    job: usize,
    /// What has been picked by hand for a slot, over whatever the attire puts there, and which
    /// slot's picker is open. Both index [`menus::Creator::pieces`].
    chosen: [Option<usize>; 5],
    picking: Option<Slot>,
    search: [String; 5],
    matched: RefCell<[(Option<String>, Vec<usize>); 5]>,
    matcher: FuzzyMatcher,
    /// The models each set is worn as under the current code, by slot. The picker asks about every
    /// set it lists, and a directory listing is too dear to pay for one on every frame.
    sets: RefCell<BTreeMap<u16, [Option<String>; 5]>>,
    /// The files the model on screen was built from, so a pick that changes nothing costs nothing.
    worn: Vec<(String, u16)>,
    /// Every file read so far, so a change of clothes only asks for what it newly needs.
    held: Files,
    fetching: Option<TrackedPromise<Result<Read>>>,
    model: Option<Result<Box<mdl::Rendered>, String>>,
}

impl Default for CharacterBuilder {
    fn default() -> Self {
        Self {
            listing: None,
            codes: Vec::new(),
            creator: menus::Creator::default(),
            reading: None,
            reading_pieces: None,
            race: 1,
            tribe: 1,
            female: false,
            code: 101,
            body: Vec::new(),
            faces: Vec::new(),
            hairs: Vec::new(),
            face: 1,
            hair: 1,
            attire: Attire::default(),
            job: 0,
            chosen: [None; 5],
            picking: None,
            search: Default::default(),
            matched: Default::default(),
            matcher: FuzzyMatcher::new(),
            sets: RefCell::new(BTreeMap::new()),
            worn: Vec::new(),
            held: Files::new(),
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
        self.reading_pieces = None;
        self.body.clear();
        self.faces.clear();
        self.hairs.clear();
        // What was picked by hand is where a piece sat in a list that is about to be read again.
        self.chosen = [None; 5];
        self.matched.take();
        self.sets.borrow_mut().clear();
        self.worn.clear();
        self.held.clear();
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
        if let Some(promise) = self.reading.take() {
            match promise.try_take() {
                Ok(Ok(read)) => {
                    self.creator = read;
                    self.faces.clear();
                    // Only once the character is dressed: both walk the same sheet, and the one
                    // that gets there first is the one that finishes first.
                    let backend = backend.clone();
                    let language = LANGUAGE.get(ctx);
                    self.reading_pieces = Some(TrackedPromise::spawn_local(async move {
                        menus::pieces(&backend, language).await
                    }));
                }
                Ok(Err(why)) => self.model = Some(Err(why.to_string())),
                Err(promise) => self.reading = Some(promise),
            }
        }
        if let Some(promise) = self.reading_pieces.take() {
            match promise.try_take() {
                Ok(Ok(read)) => {
                    self.creator.pieces = read;
                    // A picker open while they were still arriving matched against nothing.
                    self.matched.take();
                }
                Ok(Err(why)) => log::warn!("character: nothing to pick equipment from: {why}"),
                Err(promise) => self.reading_pieces = Some(promise),
            }
        }

        if self.faces.is_empty() {
            let code = resolve(
                &listing,
                &self.codes,
                &self.creator,
                self.tribe,
                self.female,
            );
            // Which model a set is worn as is the code's to say, so the answers held for the last
            // one say nothing about this one.
            if code != self.code {
                self.sets.borrow_mut().clear();
            }
            self.code = code;
            self.body = body(&listing, self.code);
            self.faces = sets(&listing, &self.code, "face");
            self.hairs = sets(&listing, &self.code, "hair");
            self.face = pick(&self.faces, self.face);
            self.hair = pick(&self.hairs, self.hair);
        }

        let wanted = self.wearing(&listing);
        if wanted != self.worn && !wanted.is_empty() {
            self.worn = wanted;
            let missing: Vec<String> = self
                .worn
                .iter()
                .map(|(path, _)| path)
                .filter(|path| !self.held.contains_key(*path))
                .cloned()
                .collect();
            match missing.is_empty() {
                true => self.dress(),
                false => {
                    let files = backend.files().clone();
                    self.fetching = Some(TrackedPromise::spawn_local(async move {
                        let mut read = Vec::with_capacity(missing.len());
                        for path in missing {
                            let bytes = files.read(&path).await?;
                            read.push((path, bytes));
                        }
                        Ok(read)
                    }));
                }
            }
        }
        if matches!(&self.fetching, Some(promise) if promise.try_get().is_some())
            && let Some(promise) = self.fetching.take()
            && let Some(read) = promise.try_get()
        {
            match read {
                Ok(read) => {
                    self.held.extend(read.iter().cloned());
                    self.dress();
                }
                Err(why) => self.model = Some(Err(why.to_string())),
            }
        }
    }

    /// The piece picked by hand for a slot, if the list it was picked from is still the one held.
    fn picked(&self, slot: Slot) -> Option<&menus::Piece> {
        self.creator.pieces[slot as usize].get(self.chosen[slot as usize]?)
    }

    /// What the character is dressed in: the attire, then anything picked by hand over it, then
    /// the slots those pieces cover themselves, which draw nothing at all rather than falling back
    /// to the body's own model. A slot picked for is never covered, since a pick is an instruction.
    fn dressed(&self) -> (Outfit, [bool; 5]) {
        let mut outfit = self.outfit();
        let mut hidden = [false; 5];
        for slot in Slot::ALL {
            if let Some(piece) = self.picked(slot) {
                outfit[slot as usize] = Some(piece.gear);
            }
        }
        for slot in Slot::ALL {
            let Some(piece) = self.picked(slot) else {
                continue;
            };
            for (at, covered) in piece.hides.iter().enumerate() {
                if *covered && self.chosen[at].is_none() {
                    outfit[at] = None;
                    hidden[at] = true;
                }
            }
        }
        (outfit, hidden)
    }

    /// The outfit the picked attire dresses the character in.
    fn outfit(&self) -> Outfit {
        match self.attire {
            Attire::Race => self
                .creator
                .attire
                .get(&(self.race, self.female))
                .copied()
                .unwrap_or_default(),
            Attire::Job => self
                .creator
                .jobs
                .get(self.job)
                .map(|job| job.outfit)
                .unwrap_or_default(),
            Attire::Smallclothes => {
                let mut outfit = Outfit::default();
                for slot in Slot::RACIAL {
                    outfit[slot as usize] = Some(SMALLCLOTHES);
                }
                outfit
            }
        }
    }

    /// Every model the character is drawn from, each with the variant it is worn at. A slot draws
    /// exactly one of them: the equipment worn in it where there is any, and the body's own model
    /// for that slot otherwise. Those two are the very same mesh wherever a race's smallclothes are
    /// its bare skin, which is what drawing both of them showed as z-fighting.
    ///
    /// The face leads, since the first file is what names the skeleton the rest are posed on and a
    /// piece of equipment worn by a race that has no model of its own is filed under another's code.
    fn wearing(&self, listing: &Listing) -> Vec<(String, u16)> {
        if self.body.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<_> = held(&self.faces, self.face)
            .into_iter()
            .chain(held(&self.hairs, self.hair))
            .map(|path| (path, 0))
            .collect();
        let (outfit, hidden) = self.dressed();
        for slot in Slot::ALL {
            let worn = outfit[slot as usize].and_then(|gear| {
                self.worn_as(listing, gear.set)[slot as usize]
                    .clone()
                    .map(|path| (path, gear.variant))
            });
            match worn {
                Some(part) => found.push(part),
                // Nothing stands in for a bare head: the body ships no model for it, and the face
                // and the hair are what draw one.
                None if hidden[slot as usize] => {}
                None => found.extend(part(&self.body, slot).map(|path| (path, 0))),
            }
        }
        found
    }

    /// The model each slot of a set is worn as under the current code, answered out of the memo
    /// and read off the listing the first time a set is asked about.
    fn worn_as(&self, listing: &Listing, set: u16) -> Ref<'_, [Option<String>; 5]> {
        if !self.sets.borrow().contains_key(&set) {
            let found = equipment(listing, self.code, set);
            self.sets.borrow_mut().insert(set, found);
        }
        Ref::map(self.sets.borrow(), |sets| &sets[&set])
    }

    /// Puts what has arrived on screen, keeping the character that is already there where there is
    /// one so a change of clothes neither moves the view nor asks for anything twice.
    fn dress(&mut self) {
        let parts: Vec<_> = self
            .worn
            .iter()
            .filter_map(|(path, variant)| {
                Some(mdl::Source {
                    path: path.clone(),
                    bytes: self.held.get(path)?.clone(),
                    variant: *variant,
                })
            })
            .collect();
        if parts.len() != self.worn.len() {
            return;
        }
        match &mut self.model {
            Some(Ok(model)) => {
                if let Err(why) = model.redress(&parts) {
                    self.model = Some(Err(why.to_string()));
                }
            }
            _ => {
                self.model = Some(
                    mdl::compose(&parts)
                        .map(Box::new)
                        .map_err(|why| why.to_string()),
                )
            }
        }
    }

    /// One slot to dress: what is in it, and, while its picker is open, everything the game names
    /// that could be. A piece the code has no model of would draw the body's own part instead of
    /// what was asked for, so it is offered but not pickable; one the game bars this race or
    /// gender from is picked all the same, since only the game bars it and the files do not.
    fn slot_ui(
        &mut self,
        ui: &mut egui::Ui,
        backend: &Backend,
        icons: &IconManager,
        listing: &Listing,
        slot: Slot,
    ) {
        let at = slot as usize;
        let (outfit, hidden) = self.dressed();
        let worn = match (
            outfit[at]
                .and_then(|gear| self.creator.pieces[at].iter().find(|piece| piece.gear == gear)),
            outfit[at],
        ) {
            (Some(piece), _) => piece.name.clone(),
            (None, Some(gear)) => format!("Set {}", gear.set),
            (None, None) => match hidden[at] {
                true => "Covered".to_owned(),
                false => "Bare".to_owned(),
            },
        };
        let open = self.picking == Some(slot);
        if ui
            .selectable_label(open, format!("{}: {worn}", slot.name()))
            .clicked()
        {
            self.picking = (!open).then_some(slot);
        }
        if !open {
            return;
        }
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut self.search[at])
                    .hint_text("Search")
                    .desired_width(ui.available_width() - 60.0),
            );
            if ui
                .add_enabled(self.chosen[at].is_some(), egui::Button::new("Attire"))
                .on_hover_text("Wear what the attire puts here")
                .clicked()
            {
                self.chosen[at] = None;
            }
        });

        let mut picked = None;
        {
            let query = self.search[at].clone();
            let matched = self.matches(slot, &query);
            let step = PIECE + 2.0 * ui.spacing().button_padding.y + ui.spacing().item_spacing.y;
            ScrollArea::vertical()
                .id_salt(("character_pieces", at))
                .max_height(step * SHOWN as f32)
                .show_rows(ui, step, matched.len(), |ui, rows| {
                    for row in rows {
                        let index = matched[row];
                        let piece = &self.creator.pieces[at][index];
                        let held = self.worn_as(listing, piece.gear.set)[at].is_some();
                        let suits = piece.suits(self.race, self.female);
                        let name = match suits {
                            true => RichText::new(&piece.name),
                            false => RichText::new(&piece.name).color(Color32::KHAKI),
                        };
                        let icon = get_icon_path(backend.icons(), piece.icon, false, Language::None);
                        let excel = backend.excel().clone();
                        let source = icons.get_or_insert_icon(&icon, ui.ctx(), || {
                            let icon = icon.clone();
                            TrackedPromise::spawn_local(async move { excel.get_icon(&icon).await })
                        });
                        let button = match source {
                            ManagedIcon::Loaded(source) => egui::Button::image_and_text(
                                egui::Image::new(source)
                                    .maintain_aspect_ratio(true)
                                    .fit_to_exact_size(egui::Vec2::splat(PIECE)),
                                name,
                            ),
                            _ => egui::Button::new(name),
                        };
                        // One line to a row, since the rows are scrolled by a fixed step and a name
                        // long enough to wrap would walk the list out from under it.
                        let response = ui.add_enabled(
                            held,
                            button
                                .truncate()
                                .selected(self.chosen[at] == Some(index))
                                .min_size(egui::vec2(ui.available_width(), PIECE)),
                        );
                        let response = match (held, suits) {
                            (false, _) => {
                                response.on_disabled_hover_text("This body has no model of it")
                            }
                            (_, false) => response
                                .on_hover_text("The game does not offer this to this race and gender"),
                            _ => response,
                        };
                        if response.clicked() {
                            picked = Some(index);
                        }
                    }
                });
        }
        if let Some(index) = picked {
            self.chosen[at] = Some(index);
        }
    }

    /// Which of a slot's pieces its search names, kept since matching every name again on every
    /// frame costs more than reading them all did.
    fn matches(&self, slot: Slot, query: &str) -> Ref<'_, Vec<usize>> {
        let at = slot as usize;
        if self.matched.borrow()[at].0.as_deref() != Some(query) {
            let found = self.matcher.match_list_indirect(
                (!query.is_empty()).then_some(query),
                self.creator.pieces[at]
                    .iter()
                    .enumerate()
                    .map(|(index, piece)| (index, piece.name.as_str())),
                |piece| piece.1,
            );
            self.matched.borrow_mut()[at] = (
                Some(query.to_owned()),
                found.into_iter().map(|(index, _)| index).collect(),
            );
        }
        Ref::map(self.matched.borrow(), |matched| &matched[at].1)
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, backend: &Backend, icons: &IconManager) {
        let listing = self.listing.clone();
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
                    ui.label(RichText::new("Attire").strong());
                    ui.horizontal(|ui| {
                        for (attire, name) in [
                            (Attire::Race, "Race"),
                            (Attire::Job, "Job"),
                            (Attire::Smallclothes, "Smallclothes"),
                        ] {
                            if ui.selectable_label(self.attire == attire, name).clicked() {
                                picked = Some(Pick::Attire(attire));
                            }
                        }
                    });
                    if self.attire == Attire::Job {
                        for (at, job) in self.creator.jobs.iter().enumerate() {
                            if ui.selectable_label(self.job == at, &job.name).clicked() {
                                picked = Some(Pick::Job(at));
                            }
                        }
                    }
                    if let Some(listing) = &listing {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Equipment").strong());
                            if self.reading_pieces.is_some() {
                                ui.spinner();
                            }
                        });
                        for slot in Slot::ALL {
                            self.slot_ui(ui, backend, icons, listing, slot);
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Face").strong());
                    grid(ui, "character_faces", &self.faces, |ui, set| {
                        let body = self.creator.body(self.tribe, self.female);
                        let icon = body.and_then(|body| body.faces.get(&set.id));
                        chip(ui, backend, icons, set.id, self.face, icon)
                            .then_some(Pick::Face(set.id))
                    })
                    .inspect(|face| picked = Some(*face));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Hair").strong());
                    grid(ui, "character_hairs", &self.hairs, |ui, set| {
                        let body = self.creator.body(self.tribe, self.female);
                        let icon = body.and_then(|body| body.hairs.get(&set.id));
                        chip(ui, backend, icons, set.id, self.hair, icon)
                            .then_some(Pick::Hair(set.id))
                    })
                    .inspect(|hair| picked = Some(*hair));
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
            Some(Pick::Attire(attire)) => self.attire = attire,
            Some(Pick::Job(job)) => self.job = job,
            Some(Pick::Face(face)) => self.face = face,
            Some(Pick::Hair(hair)) => self.hair = hair,
            None => {}
        }
    }
}

#[derive(Clone, Copy)]
enum Pick {
    Race(u32),
    Tribe(u32),
    Gender(bool),
    Attire(Attire),
    Job(usize),
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

/// The model a code wears a set as, by slot. A set is one directory, so the whole of it is listed
/// once and answered for every slot at once.
///
/// Not every race has one of its own: the ones that do not are drawn from the base body of their
/// gender, which the game then deforms onto their own build. The fallback is taken here; the
/// deformation is not, so a piece worn this way is the right garment on the wrong build.
fn equipment(listing: &Listing, code: u16, set: u16) -> [Option<String>; 5] {
    let under = format!("chara/equipment/e{set:04}/model");
    let held = listing.under(&under);
    let base = match code % 2 {
        1 => 101,
        _ => 201,
    };
    Slot::ALL.map(|slot| {
        [code, base].into_iter().find_map(|code| {
            let path = format!("{under}/c{code:04}e{set:04}_{}.mdl", slot.suffix());
            held.contains(&path).then_some(path)
        })
    })
}

/// The body a code is built on, which is the lowest set it ships: only one code carries `b0001`,
/// and the rest start wherever their own files do.
fn body(listing: &Listing, code: u16) -> Vec<String> {
    sets(listing, &code, "body")
        .first()
        .map(|set| set.parts.clone())
        .unwrap_or_default()
}

/// Which of a set's models covers one slot.
fn part(parts: &[String], slot: Slot) -> Option<String> {
    let tail = format!("_{}.mdl", slot.suffix());
    parts.iter().find(|path| path.ends_with(&tail)).cloned()
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

/// Sets to pick from, laid out as many to a row as the panel is wide enough for. Every cell is the
/// same size whether or not the creator offers an icon for what is in it, so one it does not name
/// leaves a gap in the numbering rather than a break in the grid.
fn grid<T, R>(
    ui: &mut egui::Ui,
    id: &str,
    sets: &[T],
    mut cell: impl FnMut(&mut egui::Ui, &T) -> Option<R>,
) -> Option<R> {
    let step = ICON + GAP + ui.spacing().button_padding.x * 2.0;
    let columns = ((ui.available_width() / step) as usize).max(1);
    egui::Grid::new(id)
        .spacing(egui::Vec2::splat(GAP))
        .show(ui, |ui| {
            let mut picked = None;
            for (at, set) in sets.iter().enumerate() {
                if at > 0 && at % columns == 0 {
                    ui.end_row();
                }
                picked = cell(ui, set).or(picked);
            }
            picked
        })
        .inner
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
        return numbered(ui, id, current, "No icon");
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
        // An icon that has not landed yet is not one the creator never named, and saying so would
        // have every chip claim it has no icon for as long as the icons take to arrive.
        ManagedIcon::Failed(_) => numbered(ui, id, current, "No icon"),
        _ => numbered(ui, id, current, "Loading"),
    }
}

fn numbered(ui: &mut egui::Ui, id: u16, current: u16, why: &str) -> bool {
    ui.add_sized(
        egui::Vec2::splat(ICON),
        egui::Button::new(id.to_string()).selected(current == id),
    )
    .on_hover_text(why)
    .clicked()
}
