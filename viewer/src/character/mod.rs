//! Dressing a character out of the files its body, face and hair are filed under.
//!
//! A code names a body, and everything that body can wear sits under it: `obj/body`, `obj/face` and
//! `obj/hair` each hold a numbered set, and a set's `model` directory holds every piece of it. What
//! a set is made of is read from that directory rather than from a list of suffixes, since a face is
//! several models and which ones it carries is the file tree's to say.
//!
//! What each set is offered under comes from the creator's own menus, in [`menus`].

mod emotes;
mod menus;
mod palette;

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

/// Which customisation each of the creator's menus drives, as `Customize` numbers them. Every one
/// of these is measured from `CharaMakeType` rather than named by any file.
const FACE: u32 = 5;
const HAIRSTYLE: u32 = 6;
const SKIN_COLOR: u32 = 8;
const EYE_COLOR: u32 = 9;
const HAIR_COLOR: u32 = 10;
const FEATURES: u32 = 12;
const LIP_COLOR: u32 = 20;
const FACE_PAINT_COLOR: u32 = 25;
const HEIGHT: u32 = 3;

/// The parts of a face the creator deforms, and the shape keys each is named with. A choice picks
/// the nth shape the model declares for that part, counting the first choice as the face's own.
const SHAPED: [(u32, &str); 6] = [
    (14, "shp_brw"),
    (16, "shp_eye"),
    (15, "shp_irs"),
    (19, "shp_mth"),
    (17, "shp_nse"),
    (18, "shp_chk"),
];

/// What a face calls the parts a facial feature draws as, one letter each. The creator splits them
/// across two menus and the model declares them as one run.
const FEATURE: &str = "atr_fv_";
const FEATURE_LETTERS: [char; 7] = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];

/// How big a colour swatch is drawn.
const SWATCH: f32 = 18.0;

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
    /// What has been picked for each of the creator's menus, by the `Customize` it drives. A menu
    /// the map says nothing about is at its first choice, which is what the row's own defaults are.
    choices: BTreeMap<u32, u32>,
    /// The colours the creator offers, read once.
    made: Option<palette::Made>,
    reading_made: Option<TrackedPromise<Result<palette::Made>>>,
    /// The emotes the game names, and which of them is being played.
    emotes: Vec<emotes::Emote>,
    reading_emotes: Option<TrackedPromise<Result<Vec<emotes::Emote>>>>,
    emote: Option<usize>,
    emote_search: String,
    emotes_matched: RefCell<(Option<String>, Vec<usize>)>,
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
            choices: BTreeMap::new(),
            made: None,
            reading_made: None,
            emotes: Vec::new(),
            reading_emotes: None,
            emote: None,
            emote_search: String::new(),
            emotes_matched: Default::default(),
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
        self.made = None;
        self.reading_made = None;
        self.emotes.clear();
        self.reading_emotes = None;
        self.emote = None;
        self.emotes_matched.take();
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
            let language = LANGUAGE.get(ctx);
            let creator = backend.clone();
            self.reading = Some(TrackedPromise::spawn_local(async move {
                menus::read(&creator, language).await
            }));
            let colors = backend.clone();
            self.reading_made = Some(TrackedPromise::spawn_local(async move {
                palette::Made::read(&colors).await
            }));
            let played = backend.clone();
            self.reading_emotes = Some(TrackedPromise::spawn_local(async move {
                emotes::read(&played, language).await
            }));
        }
        if let Some(promise) = self.reading_emotes.take() {
            match promise.try_take() {
                Ok(Ok(read)) => {
                    self.emotes = read;
                    self.emotes_matched.take();
                }
                Ok(Err(why)) => log::warn!("character: no emotes to play: {why}"),
                Err(promise) => self.reading_emotes = Some(promise),
            }
        }
        if let Some(promise) = self.reading_made.take() {
            match promise.try_take() {
                Ok(Ok(read)) => self.made = Some(read),
                Ok(Err(why)) => log::warn!("character: no colours to pick from: {why}"),
                Err(promise) => self.reading_made = Some(promise),
            }
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

        // Cheap enough to hand over on every frame: it walks the parts of one character and the
        // model keeps what it was already at, so nothing is rebuilt where nothing was picked.
        if let Some(Ok(model)) = &self.model {
            let (customize, hidden, shapes, stature) = self.made();
            model.made(customize, hidden, shapes, stature);
        }
    }

    /// What the creator's menus have been left at, and what the shaders and the model make of it:
    /// the colours to tint with, the parts to leave undrawn and the shape keys to deform by.
    fn made(&self) -> (mdl::Customize, BTreeSet<String>, BTreeSet<String>, f32) {
        let mut customize = mdl::Customize::default();
        // Every feature the face declares, less the ones the creator has been left on.
        let mut hidden: BTreeSet<String> = FEATURE_LETTERS
            .iter()
            .map(|letter| format!("{FEATURE}{letter}"))
            .collect();
        let mut shapes = BTreeSet::new();
        let mut stature = 1.0;
        let Some(body) = self.creator.body(self.tribe, self.female) else {
            return (customize, hidden, shapes, stature);
        };
        let palettes = self
            .made
            .as_ref()
            .map(|made| made.palettes(self.tribe, self.female));
        for menu in &body.menus {
            let at = self.choice(menu) as usize;
            if let Some(palettes) = &palettes {
                let color = match menu.customize {
                    SKIN_COLOR => Some((&palettes.skin, &mut customize.skin)),
                    HAIR_COLOR => Some((&palettes.hair, &mut customize.hair)),
                    LIP_COLOR => Some((&palettes.lips, &mut customize.lip)),
                    EYE_COLOR => Some((&palettes.eyes, &mut customize.right_eye)),
                    _ => None,
                };
                if let Some((swatches, held)) = color {
                    *held = swatches.shaded(at);
                }
                if menu.customize == EYE_COLOR {
                    customize.left_eye = customize.right_eye;
                }
                if menu.customize == FACE_PAINT_COLOR {
                    let [red, green, blue, _] = palettes.face_paint.shaded(at);
                    customize.option = [red, green, blue];
                }
            }
            if menu.customize == HEIGHT
                && let Some(palettes) = &palettes
            {
                let [short, tall] = palettes.height;
                let last = menu.count.saturating_sub(1).max(1) as f32;
                stature = short + (tall - short) * (at as f32 / last);
            }
            if menu.customize == FEATURES {
                // The two menus that share this one number are halves of the same run of parts,
                // and each states where in it its own toggles start.
                let first = body
                    .menus
                    .iter()
                    .take_while(|held| !std::ptr::eq(*held, menu))
                    .filter(|held| held.customize == FEATURES)
                    .map(|held| held.count as usize)
                    .sum::<usize>();
                for bit in 0..menu.count as usize {
                    if at & 1 << bit != 0 && let Some(letter) = FEATURE_LETTERS.get(first + bit) {
                        hidden.remove(&format!("{FEATURE}{letter}"));
                    }
                }
            }
            if let Some((_, prefix)) = SHAPED.iter().find(|(held, _)| *held == menu.customize)
                && at > 0
                && let Some(letter) = FEATURE_LETTERS.get(at - 1)
            {
                shapes.insert(format!("{prefix}_{letter}"));
            }
        }
        (customize, hidden, shapes, stature)
    }

    /// Where a menu has been left, which is its first choice until it is picked from.
    fn choice(&self, menu: &menus::Menu) -> u32 {
        self.choices
            .get(&menu.customize)
            .copied()
            .unwrap_or(0)
            .min(menu.count.saturating_sub(1))
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

    /// Everything the creator offers this body, in its own order and under its own names. A face
    /// and a hairstyle name the files the character is built from, so those are kept where the
    /// rest of the choices are not.
    fn appearance(
        &mut self,
        ui: &mut egui::Ui,
        backend: &Backend,
        icons: &IconManager,
    ) -> Option<Pick> {
        let body = self.creator.body(self.tribe, self.female).cloned()?;
        let palettes = self
            .made
            .as_ref()
            .map(|made| made.palettes(self.tribe, self.female));
        let mut picked = None;
        for (at, menu) in body.menus.iter().enumerate() {
            ui.add_space(8.0);
            ui.label(RichText::new(&menu.name).strong());
            let current = self.choice(menu);
            match menu.kind {
                menus::Kind::Slider => {
                    let mut held = current;
                    let last = menu.count.saturating_sub(1);
                    if ui.add(egui::Slider::new(&mut held, 0..=last)).changed() {
                        picked = Some(Pick::Made(menu.customize, held));
                    }
                }
                menus::Kind::Features => {
                    ui.horizontal_wrapped(|ui| {
                        for bit in 0..menu.count {
                            let on = current & 1 << bit != 0;
                            if ui.selectable_label(on, (bit + 1).to_string()).clicked() {
                                picked = Some(Pick::Made(menu.customize, current ^ 1 << bit));
                            }
                        }
                    });
                }
                menus::Kind::Skin | menus::Kind::Eyes => {
                    let swatches = palettes.as_ref().map(|held| match menu.customize {
                        SKIN_COLOR => &held.skin,
                        HAIR_COLOR => &held.hair,
                        EYE_COLOR => &held.eyes,
                        LIP_COLOR => &held.lips,
                        FACE_PAINT_COLOR => &held.face_paint,
                        _ => &held.features,
                    });
                    let Some(swatches) = swatches else {
                        ui.spinner();
                        continue;
                    };
                    let offered = (menu.count as usize).min(swatches.len());
                    egui::Grid::new(("character_colors", at))
                        .spacing(egui::Vec2::splat(2.0))
                        .show(ui, |ui| {
                            for index in 0..offered {
                                if index > 0 && index % palette::COLUMNS == 0 {
                                    ui.end_row();
                                }
                                let Some(color) = swatches.shown(index) else {
                                    continue;
                                };
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::Vec2::splat(SWATCH),
                                    egui::Sense::click(),
                                );
                                ui.painter().rect_filled(rect, 2.0, color);
                                if current as usize == index {
                                    ui.painter().rect_stroke(
                                        rect,
                                        2.0,
                                        ui.visuals().selection.stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                if response.clicked() {
                                    picked = Some(Pick::Made(menu.customize, index as u32));
                                }
                            }
                        });
                }
                menus::Kind::Icons | menus::Kind::Listed => {
                    let choices: Vec<Choice> = (0..menu.count)
                        .map(|index| self.choice_of(menu, index))
                        .collect();
                    let current = match menu.customize {
                        FACE => choices.iter().position(|held| held.id == self.face),
                        HAIRSTYLE => choices.iter().position(|held| held.id == self.hair),
                        _ => Some(current as usize),
                    }
                    .unwrap_or(usize::MAX);
                    grid(ui, &format!("character_menu_{at}"), &choices, |ui, held| {
                        let selected = choices
                            .iter()
                            .position(|other| std::ptr::eq(other, held))
                            .is_some_and(|index| index == current);
                        chip(ui, backend, icons, held, selected)
                            .then_some(Pick::Choice(menu.customize, held.at, held.id))
                    })
                    .inspect(|choice| picked = Some(*choice));
                }
            }
        }
        picked
    }

    /// The emotes the game names, searched by name and drawn under their own icons. Standing and
    /// its unique variant are here rather than in a control of their own: both are idles, so both
    /// are looked up exactly as an emote is.
    fn emotes_ui(
        &mut self,
        ui: &mut egui::Ui,
        backend: &Backend,
        icons: &IconManager,
    ) -> Option<Pick> {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Emote").strong());
            if self.reading_emotes.is_some() {
                ui.spinner();
            }
        });
        ui.add(
            TextEdit::singleline(&mut self.emote_search)
                .hint_text("Search")
                .desired_width(f32::INFINITY),
        );
        let mut picked = None;
        let query = self.emote_search.clone();
        let matched = self.emotes_matching(&query);
        let step = PIECE + 2.0 * ui.spacing().button_padding.y + ui.spacing().item_spacing.y;
        ScrollArea::vertical()
            .id_salt("character_emotes")
            .max_height(step * SHOWN as f32)
            .show_rows(ui, step, matched.len(), |ui, rows| {
                for row in rows {
                    let index = matched[row];
                    let emote = &self.emotes[index];
                    let path = get_icon_path(backend.icons(), emote.icon, false, Language::None);
                    let excel = backend.excel().clone();
                    let source = icons.get_or_insert_icon(&path, ui.ctx(), || {
                        let path = path.clone();
                        TrackedPromise::spawn_local(async move { excel.get_icon(&path).await })
                    });
                    let button = match source {
                        ManagedIcon::Loaded(source) => egui::Button::image_and_text(
                            egui::Image::new(source)
                                .maintain_aspect_ratio(true)
                                .fit_to_exact_size(egui::Vec2::splat(PIECE)),
                            &emote.name,
                        ),
                        _ => egui::Button::new(&emote.name),
                    };
                    if ui
                        .add(
                            button
                                .truncate()
                                .selected(self.emote == Some(index))
                                .min_size(egui::vec2(ui.available_width(), PIECE)),
                        )
                        .clicked()
                    {
                        picked = Some(Pick::Emote(index));
                    }
                }
            });
        picked
    }

    /// Which emotes a search names, kept the way a slot's own list is.
    fn emotes_matching(&self, query: &str) -> Ref<'_, Vec<usize>> {
        if self.emotes_matched.borrow().0.as_deref() != Some(query) {
            let found = self.matcher.match_list_indirect(
                (!query.is_empty()).then_some(query),
                self.emotes
                    .iter()
                    .enumerate()
                    .map(|(index, emote)| (index, emote.name.as_str())),
                |emote| emote.1,
            );
            *self.emotes_matched.borrow_mut() = (
                Some(query.to_owned()),
                found.into_iter().map(|(index, _)| index).collect(),
            );
        }
        Ref::map(self.emotes_matched.borrow(), |(_, rows)| rows)
    }

    /// What one choice of a menu is: the number the file tree uses for it, and the icon it is
    /// offered under. A menu either names icons outright or names rows that carry one.
    fn choice_of(&self, menu: &menus::Menu, index: u32) -> Choice {
        let param = menu.params.get(index as usize).copied().unwrap_or(0);
        let (id, icon) = match self.creator.offered.get(&(param.max(0) as u32)) {
            Some((id, icon)) => (*id, *icon),
            None => (
                menus::face(param).unwrap_or(index as u16 + 1),
                param.max(0) as u32,
            ),
        };
        Choice {
            at: index,
            id,
            icon: (icon > 0).then_some(icon),
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
                    self.appearance(ui, backend, icons)
                        .inspect(|made| picked = Some(*made));
                    self.emotes_ui(ui, backend, icons)
                        .inspect(|emote| picked = Some(*emote));
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
            Some(Pick::Emote(emote)) => {
                self.emote = Some(emote);
                if let (Some(Ok(model)), Some(emote)) = (&self.model, self.emotes.get(emote))
                    && let Some(path) = emote.pack(self.code, 0)
                {
                    model.play(&path);
                }
            }
            Some(Pick::Made(customize, choice)) => {
                self.choices.insert(customize, choice);
            }
            Some(Pick::Choice(customize, choice, id)) => {
                self.choices.insert(customize, choice);
                match customize {
                    FACE => self.face = id,
                    HAIRSTYLE => self.hair = id,
                    _ => {}
                }
            }
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
    /// A menu left at a choice, by the customisation it drives.
    Made(u32, u32),
    /// The same, where the choice also names the files a face or a hairstyle is built from.
    Choice(u32, u32, u16),
    Emote(usize),
}

/// One choice a menu offers: where it sits in the menu, the number the file tree files it under,
/// and the icon the creator draws it as.
struct Choice {
    at: u32,
    id: u16,
    icon: Option<u32>,
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
    choice: &Choice,
    selected: bool,
) -> bool {
    let Some(icon) = choice.icon else {
        return numbered(ui, choice, selected, "No icon");
    };
    let path = get_icon_path(backend.icons(), icon, false, Language::None);
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
                .selected(selected),
            )
            .on_hover_text(choice.id.to_string())
            .clicked(),
        // An icon that has not landed yet is not one the creator never named, and saying so would
        // have every chip claim it has no icon for as long as the icons take to arrive.
        ManagedIcon::Failed(_) => numbered(ui, choice, selected, "No icon"),
        _ => numbered(ui, choice, selected, "Loading"),
    }
}

fn numbered(ui: &mut egui::Ui, choice: &Choice, selected: bool, why: &str) -> bool {
    ui.add_sized(
        egui::Vec2::splat(ICON),
        egui::Button::new((choice.at + 1).to_string()).selected(selected),
    )
    .on_hover_text(why)
    .clicked()
}
