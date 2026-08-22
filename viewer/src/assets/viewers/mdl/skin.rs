//! Posing a model on the skeleton it is skinned to.
//!
//! A mesh's blend indices name slots of its own bone table, and that table names bones the way a
//! skeleton does, so the palette a skinned shader reads is matched up by name rather than by
//! position. Each joint carries the pose a motion puts its bone in against the pose the model is
//! stored in, which leaves a bone the skeleton does not name standing where the file put it.
//!
//! The skeleton is guessed from the model's own path and fetched on the first frame that draws a
//! skinned mesh, the way the model's `.imc` is. The packs are read off the install's own listing,
//! since nothing in the model, the skeleton or the sheets names the ones a model can play.
//!
//! A body's own skeleton names none of the bones a face, a hairstyle or a piece of headgear moves
//! on: those hang off skeletons of their own, and `.est` is what says which. They are merged into
//! the body's rather than posed apart, since each is stated as bones hanging off one the body
//! already names.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::rc::Rc;

use anyhow::Result;
use egui::{Color32, RichText};
use glam::{Mat4, Vec3};
use ironworks::file::File;
use ironworks::file::est::ExtraSkeletonTemplate;
use ironworks::file::pap::{AnimationPack, Binding};
use ironworks::file::sklb::{SkeletonBinary, Transform};

use super::super::skeleton::{Placement, Rig, middle};
use super::super::{link, placed, section};
use crate::backend::Backend;
use crate::data::listing::{Listed, Listing};
use crate::settings::api_base;
use crate::utils::{TrackedPromise, file_name};

/// What the picker calls standing the model where its own file put it.
const REST: &str = "Reference pose";
/// How tall the pack list is allowed to get. A human carries thousands of them.
const PACK_LIST_HEIGHT: f32 = 240.0;

/// The bone a body hangs off, which is what a pose is centred on. A tail carries many bones a long
/// way out and swings them, and averaging every bone instead walks the frame around with it.
const ANCHOR: &str = "n_hara";

/// The pair of bones the creator's bust slider scales, which are leaves of the body's own skeleton.
const BUST: [&str; 2] = ["j_mune_l", "j_mune_r"];

/// The bone a mount seats its rider on. Every body the game names a mount carries one, and nothing
/// else does.
const SEAT: &str = "n_mount";

/// The rig a model is skinned to, ready to answer a mesh's bone table with a palette.
pub struct Skin {
    rig: Rig,
    /// Where each bone rests, inverted: what takes a vertex out of the pose the model is stored in.
    rest: Vec<Mat4>,
    /// Which bone the skeleton calls each name.
    named: HashMap<String, usize>,
    /// The bone a pose is centred on, where it rests, and how far the furthest bone stands from it,
    /// which a pose's own are read against.
    anchor: Option<usize>,
    home: Vec3,
    spread: f32,
}

impl Skin {
    fn new(rig: Rig) -> Self {
        let world = rig.world(rig.reference());
        let rest = world
            .iter()
            .map(|placement| placement.matrix().inverse())
            .collect();
        let named: HashMap<_, _> = rig
            .names()
            .iter()
            .enumerate()
            .map(|(bone, name)| (name.clone(), bone))
            .collect();
        let anchor = named.get(ANCHOR).copied();
        let (home, spread) = middle(&world, anchor);
        Self {
            rig,
            rest,
            named,
            anchor,
            home,
            spread,
        }
    }

    /// What each slot of one mesh's bone table moves a vertex by, in the model's own space.
    fn palette(&self, table: &[String], posed: &[Placement]) -> Vec<Mat4> {
        table
            .iter()
            .map(|name| match self.named.get(name) {
                Some(bone) => posed[*bone].matrix() * self.rest[*bone],
                None => Mat4::IDENTITY,
            })
            .collect()
    }
}

/// A skeleton as its own file states it, held unbuilt so several can be merged into one rig.
struct Skeleton {
    names: Vec<String>,
    parents: Vec<i16>,
    reference: Vec<Transform>,
}

impl Skeleton {
    fn read(bytes: &[u8]) -> Result<Self> {
        let file = SkeletonBinary::read(Cursor::new(bytes.to_vec()))?;
        let skeleton = file.parse_skeleton()?;
        Ok(Self {
            names: skeleton.bones().to_vec(),
            parents: skeleton.parent_indices().to_vec(),
            reference: skeleton.reference_pose().to_vec(),
        })
    }
}

/// A skeleton a part is posed on beyond the body's own, which is one table each.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extra {
    Face,
    Hair,
    Head,
    Body,
}

impl Extra {
    const ALL: [Self; 4] = [Self::Face, Self::Hair, Self::Head, Self::Body];

    fn table(self) -> &'static str {
        match self {
            Self::Face => "chara/xls/charadb/faceSkeletonTemplate.est",
            Self::Hair => "chara/xls/charadb/hairSkeletonTemplate.est",
            Self::Head => "chara/xls/charadb/extra_met.est",
            Self::Body => "chara/xls/charadb/extra_top.est",
        }
    }

    /// The directory its skeletons are filed under, and the letter their files carry.
    fn filed(self) -> (&'static str, char) {
        match self {
            Self::Face => ("face", 'f'),
            Self::Hair => ("hair", 'h'),
            Self::Head => ("met", 'm'),
            Self::Body => ("top", 't'),
        }
    }
}

/// One frame of a model's pose, worked out once for everything that reads it.
#[derive(Default)]
pub struct Pose {
    /// The palette each mesh's blend indices read, in the model's own space.
    pub joints: Vec<Vec<Mat4>>,
    /// The rig itself, drawn where it was asked for.
    pub skeleton: Vec<placed::Batch>,
    /// How far the pose has carried the bones from where the model rests.
    pub drift: Vec3,
    /// How much further from the middle of them the pose flings the bones than the rest pose does,
    /// which the geometry hung on them reaches by too.
    pub stretch: f32,
    /// Where this rig seats a rider, for the one that is a mount.
    seat: Option<Placement>,
}

/// What a pack names the motion a model stands in, whatever rig it is built on.
const IDLE: &str = "_id0";

/// The motions a pack holds, and the name each of its animations gives one.
struct Motions {
    /// Animation names, each with the motion it plays.
    named: Vec<(String, usize)>,
    bindings: Vec<Binding>,
}

impl Motions {
    fn read(bytes: &[u8]) -> Result<Self> {
        let file = AnimationPack::read(Cursor::new(bytes.to_vec()))?;
        let bindings = file.parse_animations()?;
        let named = file
            .animations()
            .iter()
            .filter_map(|animation| {
                let motion = usize::try_from(animation.havok_index()).ok()?;
                bindings
                    .get(motion)
                    .map(|_| (animation.name().to_owned(), motion))
            })
            .collect();
        Ok(Self { named, bindings })
    }

    /// The motion the picker is on.
    fn binding(&self, motion: usize) -> Option<&Binding> {
        let (_, at) = self.named.get(motion)?;
        self.bindings.get(*at)
    }

    /// Which motion the pack opens on: the idle where it names one, since a monster's pack leads
    /// with a special rather than with the motion it stands in. Otherwise the first that stands on
    /// its own, since the first of a human's idle pack is a delta over whatever else is playing and
    /// a model posed on one alone scatters. A pack of nothing but deltas, which every facial one
    /// is, opens on its first.
    fn standing(&self) -> Option<usize> {
        let alone = |at: &usize| self.binding(*at).is_some_and(|held| held.blend_hint() == 0);
        (0..self.named.len())
            .find(|at| alone(at) && self.named[*at].0.ends_with(IDLE))
            .or_else(|| (0..self.named.len()).find(alone))
            .or((!self.named.is_empty()).then_some(0))
    }
}

/// One motion playing on the rig: the pack it comes from, which of that pack's motions, and how
/// far into it.
#[derive(Default)]
struct Layer {
    /// The pack to play, as the user or an emote has it.
    wanted: RefCell<String>,
    pack: RefCell<Option<Fetch<Motions>>>,
    /// What to hold once this pack has played through, which is how an emote states the pose it
    /// settles into apart from the motion that gets it there.
    then: RefCell<Option<String>>,
    /// Which of the pack's motions to open on, by name. A face keeps the ones it uses often in a
    /// pack together, so an expression is not always a file of its own.
    opening: RefCell<Option<String>>,
    /// Which motion is playing, indexing [`Motions::named`]. None leaves the bones where the
    /// skeleton rests, which is what a file being inspected shows.
    motion: Cell<Option<usize>>,
    time: Cell<f32>,
}

impl Layer {
    fn load(&self, path: &str, motion: Option<&str>, then: Option<&str>) {
        path.clone_into(&mut self.wanted.borrow_mut());
        *self.pack.borrow_mut() = None;
        *self.then.borrow_mut() = then.map(ToOwned::to_owned);
        *self.opening.borrow_mut() = motion.map(ToOwned::to_owned);
        self.motion.set(None);
        self.time.set(0.0);
    }

    /// Takes up the pack once it lands, opening on the motion asked for. A pack that never
    /// arrives gives way to whatever was queued behind it: not every race ships the motion an
    /// emote starts with.
    fn poll(&self, backend: &Backend) {
        let wanted = self.wanted.borrow().clone();
        let mut held = self.pack.borrow_mut();
        if wanted.is_empty() || !matches!(held.as_ref(), None | Some(Fetch::Fetching(_))) {
            return;
        }
        Fetch::poll(&mut held, backend, &wanted, Motions::read);
        let motion = held
            .as_ref()
            .and_then(Fetch::ready)
            .and_then(|motions| match self.opening.borrow().as_deref() {
                Some(name) => motions.named.iter().position(|(held, _)| held == name),
                None => motions.standing(),
            });
        let failed = matches!(held.as_ref(), Some(Fetch::Failed(_)));
        drop(held);
        self.motion.set(motion);
        if failed {
            let then = self.then.borrow_mut().take();
            if let Some(then) = then {
                self.load(&then, None, None);
            }
        }
    }

    /// Which of the pack's motions is playing, with the rest pose as the way out of playing any.
    fn motion_ui(&self, ui: &mut egui::Ui, id: &str) {
        let pack = self.pack.borrow();
        let Some(motions) = pack.as_ref().and_then(Fetch::ready) else {
            return;
        };
        let motion = self.motion.get();
        egui::ComboBox::from_id_salt(id)
            .selected_text(match motion.and_then(|at| motions.named.get(at)) {
                Some((name, _)) => name.as_str(),
                None => REST,
            })
            .show_ui(ui, |ui| {
                if ui.selectable_label(motion.is_none(), REST).clicked() {
                    self.motion.set(None);
                    self.time.set(0.0);
                }
                for (at, (name, _)) in motions.named.iter().enumerate() {
                    if ui.selectable_label(motion == Some(at), name).clicked() {
                        self.motion.set(Some(at));
                        self.time.set(0.0);
                    }
                }
            });
    }

    /// How far the motion on screen runs, or nothing if none is playing.
    fn duration(&self) -> Option<f32> {
        let pack = self.pack.borrow();
        let motions = pack.as_ref().and_then(Fetch::ready)?;
        let binding = self.motion.get().and_then(|at| motions.binding(at))?;
        Some(binding.motion().duration().max(f32::EPSILON))
    }

    /// Runs the clock on by `step`, taking up whatever was queued behind the motion once it has
    /// played through. Nothing queued means it loops, which is what a pose held forever wants.
    fn advance(&self, step: f32) {
        let Some(duration) = self.duration() else {
            return;
        };
        let time = self.time.get() + step.min(duration);
        if time <= duration {
            self.time.set(time);
            return;
        }
        let then = self.then.borrow_mut().take();
        match then {
            Some(then) => self.load(&then, None, None),
            None => self.time.set(time - duration),
        }
    }
}

/// One file on its way in, and what it decoded to.
enum Fetch<T> {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(T),
    Failed(String),
}

impl<T> Fetch<T> {
    /// Asks for `path` if nothing has, and reads it once it lands.
    fn poll(
        held: &mut Option<Self>,
        backend: &Backend,
        path: &str,
        read: impl FnOnce(&[u8]) -> Result<T>,
    ) {
        match held {
            None => {
                let files = backend.files().clone();
                let wanted = path.to_owned();
                *held = Some(Self::Fetching(TrackedPromise::spawn_local(async move {
                    files.read(&wanted).await
                })));
            }
            Some(Self::Fetching(promise)) => {
                let Some(result) = promise.try_get() else {
                    return;
                };
                let landed = result
                    .as_ref()
                    .map_err(ToString::to_string)
                    .and_then(|bytes| read(bytes).map_err(|why| why.to_string()));
                *held = Some(match landed {
                    Ok(value) => Self::Ready(value),
                    Err(why) => Self::Failed(why),
                });
            }
            Some(_) => {}
        }
    }

    fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// One pack a model can be played from, as the listing names it.
struct Pack {
    path: String,
    /// What the picker calls it: the path with everything every pack shares cut off the front.
    label: String,
}

/// What plays a model: the skeleton it is skinned to, the motions laid over it, and the clock.
pub struct Animation {
    /// The `c0101` of the model's own path, which everything it plays is filed under.
    code: Option<String>,
    /// Where the model's own path says its skeleton is, and the file that came of it.
    skeleton: Option<String>,
    base: RefCell<Option<Fetch<Skeleton>>>,
    /// Which extra skeleton each part on screen asks for, out of the parts' own file names.
    needs: RefCell<Vec<(Extra, u16)>>,
    /// The tables naming which skeleton a set is posed on, fetched only where a part needs one.
    tables: RefCell<[Option<Fetch<ExtraSkeletonTemplate>>; 4]>,
    /// Every extra skeleton asked for so far, by path, kept across a change of clothes.
    extras: RefCell<BTreeMap<String, Option<Fetch<Skeleton>>>>,
    /// The rig everything is posed on, and which extras it was built from, so it is built again as
    /// more of them land.
    skin: RefCell<Option<Skin>>,
    built: RefCell<Vec<String>>,
    /// Whether the bones this rig cannot name have been counted since it was last built.
    counted: Cell<bool>,
    /// The bodies to read packs from, nearest first, which is the model's own until it is told
    /// what it is built on.
    built_on: RefCell<Vec<String>>,
    packs: RefCell<Option<Result<Vec<Pack>, Rc<str>>>>,
    /// Cuts the pack list down while the picker is open.
    filter: RefCell<String>,
    /// What the body does, and the expression laid over it. A facial motion states a delta on
    /// bones the body's own motions never touch, so the two play at once rather than in turn.
    body: Layer,
    face: Layer,
    /// What the bust bones are scaled by, three axes in their own frame.
    bust: Cell<Vec3>,
    running: Cell<bool>,
    /// The mount the body is seated on, posed on a rig of its own. A mount names the same bones a
    /// body does, so the two cannot be merged the way an extra skeleton is.
    mounted: Option<Box<Animation>>,
}

impl Animation {
    pub fn new<'a>(models: impl IntoIterator<Item = &'a str>) -> Self {
        let models: Vec<&str> = models.into_iter().collect();
        let code = models.iter().find_map(|model| code(model));
        let mount = ridden(code.as_deref(), &models);
        let worn = worn_by(mount.as_deref(), &models);
        Self {
            skeleton: code.as_deref().and_then(skeleton_path),
            base: RefCell::new(None),
            needs: RefCell::new(needed(&worn)),
            tables: Default::default(),
            extras: Default::default(),
            skin: RefCell::new(None),
            built: Default::default(),
            counted: Cell::new(false),
            built_on: RefCell::new(code.iter().cloned().collect()),
            packs: RefCell::new(None),
            filter: RefCell::new(String::new()),
            body: Layer {
                wanted: RefCell::new(code.as_deref().and_then(pack_path).unwrap_or_default()),
                ..Default::default()
            },
            face: Default::default(),
            bust: Cell::new(Vec3::ONE),
            running: Cell::new(true),
            mounted: mount.map(|mount| Box::new(Animation::new(filed_under(&mount, &models)))),
            code,
        }
    }

    /// The `101` of `c0101`, which is what the extra skeleton tables key their answers on.
    fn body_code(&self) -> Option<u16> {
        self.code.as_deref()?.get(1..)?.parse().ok()
    }

    /// Whether a model is one this body is drawn from, which its file name states. Asked of a
    /// mount, which is the one body that never borrows a model from another.
    fn owns(&self, model: &str) -> bool {
        code(model) == self.code
    }

    /// The mount the body is seated on, where it is on one.
    pub fn rides(&self) -> Option<&str> {
        self.mounted.as_ref()?.code.as_deref()
    }

    /// Whether the rigs on hand are the ones a set of models is posed on: the body the first of
    /// them names, and the mount it is ridden on. Neither can be pointed elsewhere once built, so
    /// a change to either is what asks for a rig of its own.
    pub fn poses<'a>(&self, models: impl IntoIterator<Item = &'a str>) -> bool {
        let models: Vec<&str> = models.into_iter().collect();
        let code = models.iter().find_map(|model| code(model));
        let mount = ridden(code.as_deref(), &models);
        code == self.code && mount == self.mounted.as_ref().and_then(|held| held.code.clone())
    }

    /// Points the extra skeletons at what is being worn now, keeping everything already fetched:
    /// a hat that comes back off a picker is not worth asking for twice.
    pub fn rewear<'a>(&self, models: impl IntoIterator<Item = &'a str>) {
        let models: Vec<&str> = models.into_iter().collect();
        let mount = self.mounted.as_ref().and_then(|held| held.code.as_deref());
        if let (Some(mounted), Some(mount)) = (&self.mounted, mount) {
            mounted.rewear(filed_under(mount, &models));
        }
        *self.needs.borrow_mut() = needed(&worn_by(mount, &models));
    }

    /// Asks for the skeleton, the listing and the pack, and takes up whichever has landed. Only
    /// called for a model that carries bone indices, so nothing is fetched for one that could not
    /// be posed.
    pub fn poll(&self, ctx: &egui::Context, backend: &Backend) {
        if let Some(mounted) = &self.mounted {
            mounted.poll(ctx, backend);
        }
        if let Some(path) = &self.skeleton {
            Fetch::poll(&mut self.base.borrow_mut(), backend, path, Skeleton::read);
        }
        self.poll_extras(backend);
        let mut held = self.packs.borrow_mut();
        if held.is_none() {
            *held = match backend.listing(&api_base(ctx)) {
                Listed::Loading => None,
                Listed::Ready(listing) => Some(Ok(self.listed(&listing))),
                Listed::Failed(why) => Some(Err(why)),
            };
        }
        drop(held);
        for layer in self.layers() {
            layer.poll(backend);
        }
        self.poll_ordering(backend);
        if self.running.get() {
            let step = ctx.input(|input| input.stable_dt);
            for layer in self.layers() {
                layer.advance(step);
            }
            // Nothing else asks for a frame while the pointer is still, so playback has to.
            ctx.request_repaint();
        }
    }

    fn layers(&self) -> [&Layer; 2] {
        [&self.body, &self.face]
    }

    /// Asks for the skeleton each playing motion's tracks are ordered by. A facial motion names a
    /// face skeleton of its own, whose bones the body's skeleton does not carry.
    fn poll_ordering(&self, backend: &Backend) {
        let Some(code) = self.code.as_deref() else {
            return;
        };
        let wanted: Vec<String> = self
            .layers()
            .iter()
            .filter_map(|layer| ordering(code, &layer.wanted.borrow()))
            .collect();
        for path in wanted {
            let mut extras = self.extras.borrow_mut();
            let held = extras.entry(path.clone()).or_default();
            Fetch::poll(held, backend, &path, Skeleton::read);
        }
    }

    /// The bodies to read packs from, nearest first. A body the game files no animation under is
    /// played from the one it is built on, which is the same tree that says where it borrows its
    /// clothes from.
    pub fn built_on(&self, lineage: Vec<String>) {
        if !lineage.is_empty() {
            *self.built_on.borrow_mut() = lineage;
        }
    }

    /// The packs of the nearest body that files any, opened on the nearest body's own idle. Few
    /// bodies carry an idle at all, and one that does not is stood in the idle of the body it is
    /// built on rather than in whichever pack the listing happens to name first.
    fn listed(&self, listing: &Listing) -> Vec<Pack> {
        let mut listed: Vec<Pack> = Vec::new();
        let mut idle = None;
        for code in self.built_on.borrow().iter() {
            let Some(root) = pack_root(code) else {
                continue;
            };
            let packs = found(&root, listing.under(&root));
            if idle.is_none() {
                idle = pack_path(code).filter(|path| packs.iter().any(|pack| pack.path == *path));
            }
            if listed.is_empty() {
                listed = packs;
            }
            if idle.is_some() {
                break;
            }
        }
        // The conventional pack is right for nearly every model but not for all of them, and a
        // weapon is named none at all; either way the listing knows better.
        let mut wanted = self.body.wanted.borrow_mut();
        if !listed.iter().any(|pack| pack.path == *wanted)
            && let Some(path) = idle.or_else(|| listed.first().map(|pack| pack.path.clone()))
        {
            *wanted = path;
        }
        listed
    }

    /// Asks for the tables the parts on screen need, for the skeletons those tables name, and
    /// builds the rig again whenever another of them lands.
    fn poll_extras(&self, backend: &Backend) {
        let Some(body) = self.body_code() else {
            return;
        };
        for kind in Extra::ALL {
            if self.needs.borrow().iter().any(|(held, _)| *held == kind) {
                let mut tables = self.tables.borrow_mut();
                Fetch::poll(&mut tables[kind as usize], backend, kind.table(), |bytes| {
                    Ok(ExtraSkeletonTemplate::read(Cursor::new(bytes.to_vec()))?)
                });
            }
        }
        for path in self.named(body) {
            let mut extras = self.extras.borrow_mut();
            let held = extras.entry(path.clone()).or_default();
            Fetch::poll(held, backend, &path, Skeleton::read);
        }

        let base = self.base.borrow();
        let Some(base) = base.as_ref().and_then(Fetch::ready) else {
            return;
        };
        let extras = self.extras.borrow();
        let landed: Vec<String> = self
            .named(body)
            .into_iter()
            .filter(|path| extras[path].as_ref().and_then(Fetch::ready).is_some())
            .collect();
        if landed == *self.built.borrow() && self.skin.borrow().is_some() {
            return;
        }
        let mut rig = Rig::new(&base.names, &base.parents, &base.reference);
        for held in landed
            .iter()
            .filter_map(|path| extras[path].as_ref().and_then(Fetch::ready))
        {
            rig = rig.merged(&held.names, &held.parents, &held.reference);
        }
        *self.skin.borrow_mut() = Some(Skin::new(rig));
        *self.built.borrow_mut() = landed;
        self.counted.set(false);
    }

    /// Where every extra skeleton the parts need is filed, for the ones whose table has landed and
    /// names one. A set the table says nothing about is worn on the body's own bones.
    fn named(&self, body: u16) -> Vec<String> {
        let tables = self.tables.borrow();
        let mut found: Vec<String> = self
            .needs
            .borrow()
            .iter()
            .filter_map(|(kind, set)| {
                let id = tables[*kind as usize]
                    .as_ref()
                    .and_then(Fetch::ready)?
                    .skeleton(body, *set)
                    .filter(|id| *id > 0)?;
                let (under, letter) = kind.filed();
                Some(format!(
                    "chara/human/c{body:04}/skeleton/{under}/{letter}{id:04}/skl_c{body:04}{letter}{id:04}.sklb"
                ))
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    /// What the bust bones are scaled by, which `human.cmp` states as a pair of bounds a slider
    /// runs between.
    pub fn shaped(&self, bust: Vec3) {
        self.bust.set(bust);
    }

    /// Plays `path`, settling into `then` once it has played through.
    ///
    /// A pack of facial motions plays over whatever the body is doing rather than in place of it,
    /// so which of the two it lands on is the pack's to say.
    pub fn play(&self, path: &str, then: Option<&str>) {
        match facial(path) {
            true => &self.face,
            false => &self.body,
        }
        .load(path, None, then);
        self.running.set(true);
    }

    /// Puts an expression on the face the character wears, out of whichever pack the listing files
    /// it under: most are a pack of their own, the rest motions of the one a face keeps resident.
    /// A name filed nowhere leaves the face as it rests, which is the game's own neutral pose.
    pub fn express(&self, name: &str) {
        let Some(root) = self.face_root() else {
            return;
        };
        let file = format!("{name}.pap");
        let found = self.packs.borrow().as_ref().and_then(|packs| {
            let found = packs
                .as_ref()
                .ok()?
                .iter()
                .find(|pack| pack.path.starts_with(&root) && file_name(&pack.path) == file)?;
            Some(found.path.clone())
        });
        match found {
            Some(path) => self.face.load(&path, None, None),
            None => self.face.load(
                &format!("{root}resident/face.pap"),
                Some(&format!("cfxf_{name}")),
                None,
            ),
        }
        self.running.set(true);
    }

    /// Where the packs of the face the character wears are filed.
    fn face_root(&self) -> Option<String> {
        let code = self.code.as_deref()?;
        let body = self.body_code()?;
        let (_, set) = *self
            .needs
            .borrow()
            .iter()
            .find(|(kind, _)| *kind == Extra::Face)?;
        let id = self.tables.borrow()[Extra::Face as usize]
            .as_ref()
            .and_then(Fetch::ready)?
            .skeleton(body, set)
            .filter(|id| *id > 0)?;
        Some(format!("chara/human/{code}/animation/f{id:04}/"))
    }

    /// Where the model stands this frame: a walk of each rig it is drawn on. A mesh is posed by
    /// the rig of the body whose file it came from, and a rider's is then carried to the seat its
    /// mount names.
    pub fn pose(&self, tables: &[Vec<String>], worn: &[&str], skeleton: bool) -> Pose {
        let Some(mounted) = &self.mounted else {
            return self.walked(tables, &[], None, skeleton);
        };
        let ridden: Vec<bool> = worn.iter().map(|path| mounted.owns(path)).collect();
        let rider: Vec<bool> = ridden.iter().map(|held| !held).collect();
        let mount = mounted.walked(tables, &ridden, None, skeleton);
        let mut pose = self.walked(tables, &rider, mount.seat.as_ref(), skeleton);
        for (mesh, joints) in pose.joints.iter_mut().enumerate() {
            if ridden[mesh] {
                joints.clone_from(&mount.joints[mesh]);
            }
        }
        pose.skeleton.extend(mount.skeleton);
        // Both bodies were measured standing at the origin, so the frame is moved half the lift the
        // seat carries the rider by and widened by the other half.
        let lift = mount.seat.map_or(Vec3::ZERO, |seat| seat.translation());
        pose.drift += lift * 0.5;
        pose.stretch += lift.length() * 0.5;
        pose
    }

    /// Where one rig stands this frame, and everything read off it. `poses` says which meshes this
    /// rig answers for, the rest being another's to pose; an empty one is every mesh. `at` carries
    /// the whole rig somewhere, which is where a mount seats its rider.
    fn walked(
        &self,
        tables: &[Vec<String>],
        poses: &[bool],
        at: Option<&Placement>,
        skeleton: bool,
    ) -> Pose {
        let mine = |mesh: usize| poses.get(mesh).copied().unwrap_or(true);
        let skin = self.skin.borrow();
        let Some(skin) = skin.as_ref() else {
            return Pose {
                joints: (0..tables.len())
                    .map(|mesh| match mine(mesh) {
                        true => vec![Mat4::IDENTITY; tables[mesh].len()],
                        false => Vec::new(),
                    })
                    .collect(),
                ..Default::default()
            };
        };
        if !self.counted.replace(true) {
            // A bone the rig cannot name poses nothing and leaves its vertices where the file put
            // them, which is a face standing still while the head it hangs on turns.
            let named: Vec<&Vec<String>> = tables
                .iter()
                .enumerate()
                .filter(|(mesh, _)| mine(*mesh))
                .map(|(_, table)| table)
                .collect();
            let wanted: usize = named.iter().map(|table| table.len()).sum();
            let missing = named
                .iter()
                .flat_map(|table| table.iter())
                .filter(|name| !skin.named.contains_key(*name))
                .count();
            log::info!("mdl: {missing} of {wanted} bones are named by no skeleton");
        }
        let base = self.base.borrow();
        let extras = self.extras.borrow();
        let mut locals = skin.rig.reference().to_vec();
        for layer in self.layers() {
            let pack = layer.pack.borrow();
            let Some(binding) = layer
                .motion
                .get()
                .and_then(|motion| pack.as_ref().and_then(Fetch::ready)?.binding(motion))
            else {
                continue;
            };
            let ordered = self
                .code
                .as_deref()
                .and_then(|code| ordering(code, &layer.wanted.borrow()));
            let held = match &ordered {
                Some(path) => extras.get(path).and_then(Option::as_ref),
                None => base.as_ref(),
            };
            let Some(names) = held.and_then(Fetch::ready).map(|held| &held.names) else {
                continue;
            };
            skin.rig.lay(&mut locals, binding, names, layer.time.get());
        }
        let mut posed = skin.rig.world(&locals);
        let bust = self.bust.get();
        if bust != Vec3::ONE {
            for bone in BUST.iter().filter_map(|name| skin.named.get(*name)) {
                posed[*bone] = posed[*bone].scaled(bust);
            }
        }
        let (center, spread) = middle(&posed, skin.anchor);
        let seat = skin.named.get(SEAT).map(|bone| posed[*bone]);
        if let Some(at) = at {
            for placement in &mut posed {
                *placement = placement.carried(at);
            }
        }
        Pose {
            joints: (0..tables.len())
                .map(|mesh| match mine(mesh) {
                    true => skin.palette(&tables[mesh], &posed),
                    false => Vec::new(),
                })
                .collect(),
            skeleton: match skeleton {
                true => skin.rig.batches(&posed, None),
                false => Vec::new(),
            },
            drift: center - skin.home,
            stretch: (spread - skin.spread).max(0.0),
            seat,
        }
    }

    /// Which packs are loaded, which motion each of them plays, play and pause, and the scrubber.
    /// Only the pickers are offered until a motion is picked: with none the model stands where its
    /// own file put it, and there is nothing to play.
    pub fn ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| self.row(ui));
        if let Some(mounted) = &self.mounted {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Mount").strong());
                mounted.row(ui);
            });
        }
    }

    /// One rig's pickers and clock. Everything in here is named after the body it plays, since a
    /// mounted character draws two of these rows.
    fn row(&self, ui: &mut egui::Ui) {
        ui.push_id(self.code.as_deref().unwrap_or_default(), |ui| self.picked(ui));
    }

    fn picked(&self, ui: &mut egui::Ui) {
        self.packs_ui(ui);
        self.body.motion_ui(ui, "mdl_motion");
        self.face.motion_ui(ui, "mdl_face_motion");
        let scrubbed = match self.body.duration() {
            Some(duration) => Some((&self.body, duration)),
            None => self.face.duration().map(|duration| (&self.face, duration)),
        };
        let Some((layer, duration)) = scrubbed else {
            return;
        };
        let running = self.running.get();
        if ui.button(if running { "Pause" } else { "Play" }).clicked() {
            self.running.set(!running);
        }
        let mut time = layer.time.get().clamp(0.0, duration);
        if ui
            .add(
                egui::Slider::new(&mut time, 0.0..=duration)
                    .fixed_decimals(2)
                    .suffix(" s"),
            )
            .changed()
        {
            layer.time.set(time);
        }
    }

    /// Every pack filed under the model's own animation directory. A human carries thousands, so
    /// the list is filtered rather than scrolled.
    fn packs_ui(&self, ui: &mut egui::Ui) {
        let packs = self.packs.borrow();
        let Some(Ok(packs)) = packs.as_ref() else {
            return;
        };
        let held = [
            self.body.wanted.borrow().clone(),
            self.face.wanted.borrow().clone(),
        ];
        let mut picked = None;
        egui::ComboBox::from_id_salt("mdl_pack")
            .selected_text(match packs.iter().find(|pack| pack.path == held[0]) {
                Some(pack) => pack.label.as_str(),
                None => file_name(&held[0]),
            })
            .show_ui(ui, |ui| {
                let mut filter = self.filter.borrow_mut();
                ui.add(
                    egui::TextEdit::singleline(&mut *filter)
                        .desired_width(f32::INFINITY)
                        .hint_text("filter"),
                );
                let matching: Vec<&Pack> = packs
                    .iter()
                    .filter(|pack| pack.label.contains(&*filter))
                    .collect();
                let row = ui.text_style_height(&egui::TextStyle::Body)
                    + ui.spacing().button_padding.y * 2.0;
                egui::ScrollArea::vertical()
                    .max_height(PACK_LIST_HEIGHT)
                    .show_rows(ui, row, matching.len(), |ui, rows| {
                        for pack in &matching[rows] {
                            if ui
                                .selectable_label(held.contains(&pack.path), &pack.label)
                                .clicked()
                            {
                                picked = Some(pack.path.clone());
                            }
                        }
                    });
            });
        if let Some(path) = picked {
            self.play(&path, None);
        }
    }

    /// The files it is posed from: the skeleton it found, and the pack to take motions out of.
    pub fn details_ui(&self, ui: &mut egui::Ui, follow: &mut Option<String>) {
        section(ui, "Animation");
        match &self.skeleton {
            Some(path) => {
                if link(ui, file_name(path), path) {
                    *follow = Some(path.clone());
                }
                if let Some(Fetch::Failed(why)) = self.base.borrow().as_ref() {
                    ui.label(RichText::new(why).color(Color32::LIGHT_RED));
                }
            }
            None => {
                ui.label(RichText::new("this model's path names no skeleton").weak());
            }
        }
        let mut wanted = self.body.wanted.borrow().clone();
        if ui
            .add(egui::TextEdit::singleline(&mut wanted).hint_text("animation pack"))
            .changed()
        {
            self.body.load(&wanted, None, None);
        }
        for layer in self.layers() {
            if let Some(Fetch::Failed(why)) = layer.pack.borrow().as_ref() {
                ui.label(RichText::new(why).color(Color32::LIGHT_RED));
            }
        }
        match self.packs.borrow().as_ref() {
            Some(Ok(packs)) => {
                ui.label(RichText::new(format!("{} packs listed", packs.len())).weak());
            }
            Some(Err(why)) => {
                ui.label(RichText::new(why.as_ref()).color(Color32::LIGHT_RED));
            }
            None => {}
        }
    }
}

/// The models a mount is drawn from, which are the ones filed under its own code.
fn filed_under<'a>(mount: &str, models: &[&'a str]) -> Vec<&'a str> {
    models
        .iter()
        .copied()
        .filter(|model| code(model).as_deref() == Some(mount))
        .collect()
}

/// The models the rider is drawn from, which is everything the mount is not. A body wears models
/// filed under other bodies' codes wherever it ships none of its own, so what a rider is drawn from
/// cannot be read off the codes its files carry.
fn worn_by<'a>(mount: Option<&str>, models: &[&'a str]) -> Vec<&'a str> {
    let Some(mount) = mount else {
        return models.to_vec();
    };
    models
        .iter()
        .copied()
        .filter(|model| code(model).as_deref() != Some(mount))
        .collect()
}

/// The mount a body is being drawn seated on. Only a human rides one, and only one of them is
/// ridden at a time.
fn ridden(rig: Option<&str>, models: &[&str]) -> Option<String> {
    if !rig.is_some_and(|code| code.starts_with('c')) {
        return None;
    }
    models.iter().find_map(|model| {
        code(model).filter(|held| matches!(held.as_bytes().first(), Some(b'm' | b'd')))
    })
}

/// The `m0911` of a model's path, which is what its skeleton and its animations are filed under.
pub fn code(model: &str) -> Option<String> {
    let name = file_name(model);
    let code = name.get(..5)?;
    let (letter, digits) = code.split_at(1);
    let known = matches!(letter, "c" | "m" | "d" | "w");
    (known && digits.bytes().all(|byte| byte.is_ascii_digit())).then(|| code.to_owned())
}

/// Which extra skeleton each part asks for, out of the parts' own file names. Every model of one
/// face is posed on the same one, so the answers are worth deduplicating before they are looked up.
fn needed(models: &[&str]) -> Vec<(Extra, u16)> {
    let mut found: Vec<_> = models.iter().filter_map(|model| extra(model)).collect();
    found.dedup();
    found
}

/// What one part asks for: `c0101f0002_fac` names the face set it draws, and a piece of equipment
/// names the set it belongs to and, in its suffix, which of the two tables covers that slot.
fn extra(model: &str) -> Option<(Extra, u16)> {
    let name = file_name(model).strip_suffix(".mdl")?;
    let rest = name.get(5..)?;
    let set = rest.get(1..5)?.parse().ok()?;
    let kind = match (rest.as_bytes().first()?, rest.get(5..)?) {
        (b'f', _) => Extra::Face,
        (b'h', _) => Extra::Hair,
        (b'e', "_met") => Extra::Head,
        (b'e', "_top") => Extra::Body,
        _ => return None,
    };
    Some((kind, set))
}

/// The `f0003` of a pack filed under a face skeleton's own directory. Those hold the motions that
/// move a face, and their tracks are ordered by that skeleton's bones rather than the body's.
fn face_set(pack: &str) -> Option<&str> {
    let set = pack.split_once("/animation/")?.1.split('/').next()?;
    let named = set.len() == 5
        && set.starts_with('f')
        && set[1..].bytes().all(|byte| byte.is_ascii_digit());
    named.then_some(set)
}

/// Whether a pack moves a face rather than a body.
fn facial(pack: &str) -> bool {
    face_set(pack).is_some()
}

/// Where the skeleton a pack's tracks are ordered by is filed, or nothing where that is the
/// model's own base skeleton.
fn ordering(code: &str, pack: &str) -> Option<String> {
    let set = face_set(pack)?;
    Some(format!(
        "chara/human/{code}/skeleton/face/{set}/skl_{code}{set}.sklb"
    ))
}

/// Where the model class a code names files its skeletons and animations.
fn tree(code: &str) -> Option<&'static str> {
    match code.as_bytes().first()? {
        b'c' => Some("human"),
        b'm' => Some("monster"),
        b'd' => Some("demihuman"),
        b'w' => Some("weapon"),
        _ => None,
    }
}

fn skeleton_path(code: &str) -> Option<String> {
    let tree = tree(code)?;
    Some(format!(
        "chara/{tree}/{code}/skeleton/base/b0001/skl_{code}b0001.sklb"
    ))
}

/// Where every pack a model class can play is filed, whatever animation set names it.
fn pack_root(code: &str) -> Option<String> {
    Some(format!("chara/{}/{code}/animation/", tree(code)?))
}

/// The pack a model class idles from, which is what stands in until the listing lands. A weapon has
/// none of its own: it is moved by whoever holds it.
fn pack_path(code: &str) -> Option<String> {
    let tree = tree(code)?;
    let resident = match tree {
        "monster" => "monster",
        "weapon" => return None,
        _ => "idle",
    };
    Some(format!(
        "chara/{tree}/{code}/animation/a0001/bt_common/resident/{resident}.pap"
    ))
}

/// The packs under a model's animation directory, named by what tells them apart. Every pack of a
/// model sits under the same animation set and the same weapon class, and a segment they all share
/// says nothing; one that would leave a bare file name has gone too far.
fn found(root: &str, paths: Vec<String>) -> Vec<Pack> {
    let mut packs: Vec<Pack> = paths
        .into_iter()
        .filter_map(|path| {
            let label = path.strip_prefix(root)?.strip_suffix(".pap")?.to_owned();
            Some(Pack { path, label })
        })
        .collect();
    packs.sort_by(|left, right| left.label.cmp(&right.label));
    while let Some((head, _)) = packs.first().and_then(|pack| pack.label.split_once('/')) {
        let head = format!("{head}/");
        if !packs.iter().all(|pack| {
            pack.label
                .strip_prefix(&head)
                .is_some_and(|rest| rest.contains('/'))
        }) {
            break;
        }
        for pack in &mut packs {
            pack.label.drain(..head.len());
        }
    }
    packs
}

#[cfg(test)]
mod tests {
    use glam::{Mat4, Vec3};
    use ironworks::file::sklb::Transform;

    use super::super::super::skeleton::{Rig, middle};
    use super::{
        Extra, Skin, code, extra, facial, found, ordering, pack_path, pack_root, skeleton_path,
    };

    fn transform(translation: [f32; 3]) -> Transform {
        Transform {
            translation: [translation[0], translation[1], translation[2], 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0, 0.0],
        }
    }

    fn rig() -> Rig {
        Rig::new(
            &["n_root".to_owned(), "j_kubi".to_owned()],
            &[-1, 0],
            &[transform([0.0, 1.0, 0.0]), transform([0.0, 2.0, 0.0])],
        )
    }

    /// A pack under a face skeleton's own directory moves a face, and its tracks are ordered by
    /// that skeleton rather than by the body's.
    #[test]
    fn a_pack_filed_under_a_face_is_ordered_by_it() {
        let face = "chara/human/c0101/animation/f0003/nonresident/smile.pap";
        assert!(facial(face));
        assert_eq!(
            ordering("c0101", face).as_deref(),
            Some("chara/human/c0101/skeleton/face/f0003/skl_c0101f0003.sklb")
        );

        for body in [
            "chara/human/c0101/animation/a0001/bt_common/resident/idle.pap",
            "chara/monster/m0911/animation/a0001/bt_common/resident/monster.pap",
            "chara/weapon/w2616/animation/a0001/wp_common/resident/weapon.pap",
        ] {
            assert!(!facial(body), "{body}");
            assert_eq!(ordering("c0101", body), None, "{body}");
        }
    }

    /// A tail swinging is not the body moving.
    #[test]
    fn a_pose_stands_where_its_anchor_does_however_far_a_tail_swings() {
        let skin = Skin::new(Rig::new(
            &[
                "n_root".to_owned(),
                "n_hara".to_owned(),
                "j_sits".to_owned(),
            ],
            &[-1, 0, 1],
            &[
                transform([0.0, 0.0, 0.0]),
                transform([0.0, 1.0, 0.0]),
                transform([0.0, 0.0, -4.0]),
            ],
        ));
        assert_eq!(skin.anchor, Some(1));

        let mut locals = skin.rig.reference().to_vec();
        locals[2] = transform([3.0, 0.0, -4.0]);
        let swung = skin.rig.world(&locals);
        assert_eq!(middle(&swung, skin.anchor).0, skin.home);
        assert_ne!(
            middle(&swung, None).0,
            middle(&skin.rig.world(skin.rig.reference()), None).0
        );
    }

    /// The rest pose against itself is no movement at all, whatever order the table names the
    /// bones in, and a bone the skeleton does not name stands still.
    #[test]
    fn a_model_at_rest_stands_where_the_file_put_it() {
        let rig = rig();
        let skin = Skin::new(rig);
        let table = ["j_kubi".to_owned(), "n_root".to_owned(), "j_ago".to_owned()];
        let posed = skin.rig.world(skin.rig.reference());
        for joint in skin.palette(&table, &posed) {
            assert!(
                joint.abs_diff_eq(Mat4::IDENTITY, 1e-5),
                "a joint at rest moved: {joint}"
            );
        }
    }

    /// A bone the motion moved carries that movement, and only that bone.
    #[test]
    fn a_posed_bone_carries_what_the_pose_moved_it_by() {
        let rig = rig();
        let skin = Skin::new(rig);
        let mut locals = skin.rig.reference().to_vec();
        locals[1] = transform([0.0, 5.0, 0.0]);
        let posed = skin.rig.world(&locals);
        let held = skin.palette(&["n_root".to_owned(), "j_kubi".to_owned()], &posed);
        assert!(held[0].abs_diff_eq(Mat4::IDENTITY, 1e-5));
        assert_eq!(held[1].w_axis.truncate(), Vec3::new(0.0, 3.0, 0.0));
    }

    /// A face is skinned to bones the body's own skeleton has never heard of, and its own skeleton
    /// hangs them off one the body does name.
    #[test]
    fn a_face_bone_is_posed_once_its_own_skeleton_is_merged_in() {
        let base = rig();
        assert_eq!(base.bones(), 2);
        let merged = base.merged(
            &[
                "j_kubi".to_owned(),
                "j_f_ago".to_owned(),
                "j_nowhere".to_owned(),
                "j_f_orphan".to_owned(),
            ],
            &[-1, 0, -1, 2],
            &[
                // The head where the face's own file put it, which is nowhere near where the
                // body's chain carries it: the base's placement has to win.
                transform([0.0, 9.0, 0.0]),
                transform([0.0, 1.0, 0.0]),
                transform([0.0, 1.0, 0.0]),
                transform([0.0, 1.0, 0.0]),
            ],
        );
        // The body's own bones keep their places, since a motion's tracks name them by index, and
        // a bone hanging off nothing the merge could find is left out rather than put at the origin.
        assert_eq!(merged.names(), ["n_root", "j_kubi", "j_f_ago"]);

        let skin = Skin::new(merged);
        let mut locals = skin.rig.reference().to_vec();
        locals[1] = transform([0.0, 5.0, 0.0]);
        let posed = skin.rig.world(&locals);
        let held = skin.palette(&["j_f_ago".to_owned()], &posed);
        assert_eq!(held[0].w_axis.truncate(), Vec3::new(0.0, 3.0, 0.0));
    }

    #[test]
    fn a_part_names_the_extra_skeleton_it_is_posed_on() {
        let named = |path| extra(path).map(|(kind, set)| (kind as usize, set));
        assert_eq!(
            named("chara/human/c0101/obj/face/f0002/model/c0101f0002_fac.mdl"),
            Some((Extra::Face as usize, 2))
        );
        assert_eq!(
            named("chara/human/c0101/obj/hair/h0115/model/c0101h0115_hir.mdl"),
            Some((Extra::Hair as usize, 115))
        );
        assert_eq!(
            named("chara/equipment/e0279/model/c0101e0279_met.mdl"),
            Some((Extra::Head as usize, 279))
        );
        assert_eq!(
            named("chara/equipment/e0279/model/c0101e0279_top.mdl"),
            Some((Extra::Body as usize, 279))
        );
        // Gloves are worn on the body's own bones, and so is its own smallclothes top.
        assert_eq!(named("chara/equipment/e0279/model/c0101e0279_glv.mdl"), None);
        assert_eq!(
            named("chara/human/c0101/obj/body/b0001/model/c0101b0001_top.mdl"),
            None
        );
    }

    #[test]
    fn a_model_names_the_files_it_is_animated_from() {
        assert_eq!(
            code("chara/monster/m0911/obj/body/b0001/model/m0911b0001.mdl").as_deref(),
            Some("m0911")
        );
        assert_eq!(
            code("chara/equipment/e0971/model/c0201e0971_top.mdl").as_deref(),
            Some("c0201")
        );
        assert_eq!(
            code("bg/ffxiv/wil_w1/twn/w1t2/bgparts/w1t2_a1_bui1.mdl"),
            None
        );
        assert_eq!(
            skeleton_path("m0911").as_deref(),
            Some("chara/monster/m0911/skeleton/base/b0001/skl_m0911b0001.sklb")
        );
        assert_eq!(
            pack_path("c0101").as_deref(),
            Some("chara/human/c0101/animation/a0001/bt_common/resident/idle.pap")
        );
        assert_eq!(pack_path("w2616"), None);
        assert_eq!(
            pack_root("m0430").as_deref(),
            Some("chara/monster/m0430/animation/")
        );
    }

    /// m0430's own directory, which is the shape the pickers are named from: the set and the weapon
    /// class go, and the two `mon_sp001` under different directories stay apart.
    #[test]
    fn packs_are_named_by_what_tells_them_apart() {
        let root = "chara/monster/m0430/animation/";
        let paths = [
            "a0001/bt_common/mon_sp/m0430/hide/mon_sp001.pap",
            "a0001/bt_common/mon_sp/m0430/mon_sp001.pap",
            "a0001/bt_common/resident/monster.pap",
            "a0001/bt_common/warp/warp_start.pap",
            "a0001/bt_common/skl_m0430b0001.sklb",
        ]
        .map(|tail| format!("{root}{tail}"));

        let packs = found(root, paths.to_vec());
        assert_eq!(
            packs.iter().map(|pack| &pack.label).collect::<Vec<_>>(),
            [
                "mon_sp/m0430/hide/mon_sp001",
                "mon_sp/m0430/mon_sp001",
                "resident/monster",
                "warp/warp_start",
            ]
        );
        assert_eq!(
            packs[2].path,
            format!("{root}a0001/bt_common/resident/monster.pap")
        );
    }

    /// Trimming the shared front off one pack would leave a bare file name saying nothing.
    #[test]
    fn a_lone_pack_keeps_the_directory_that_names_it() {
        let root = "chara/weapon/w2616/animation/";
        let packs = found(
            root,
            vec![format!("{root}a0001/wp_common/resident/weapon.pap")],
        );
        assert_eq!(packs[0].label, "resident/weapon");
    }
}
