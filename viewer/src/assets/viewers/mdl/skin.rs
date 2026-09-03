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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::rc::Rc;

use anyhow::Result;
use egui::{Color32, RichText};
use glam::{Mat4, Quat, Vec3};
use ironworks::file::File;
use ironworks::file::est::ExtraSkeletonTemplate;
use ironworks::file::pap::{AnimationPack, Binding};
use ironworks::file::sklb::{SkeletonBinary, Transform};
use ironworks::file::tmb::{CommandKind, Item, Timeline};

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

/// The bones a visor hinges on, each turned about its own Z by one of the three angles the
/// gimmick states for the set. A head that names none of them raises nothing.
const VISOR: [&str; 3] = ["j_ex_met_va", "j_ex_met_vb", "j_ex_met_vc"];

/// The bone a mount seats its rider on, and the ones an extra rider is seated on beyond it. A
/// mount names them `n_mount`, then `n_mount_second` for a second seat or `n_mount_a`,
/// `n_mount_b`, ... for a third and beyond; both spellings are real, so a seat is anything
/// starting with this rather than one fixed suffix.
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
    /// Where a mount seats each of its riders, nearest first, in the order its own skeleton
    /// names them.
    seats: Vec<usize>,
}

impl Skin {
    fn new(rig: Rig) -> Self {
        let world = rig.world(rig.reference());
        let rest = world
            .iter()
            .map(|placement| placement.matrix().inverse())
            .collect();
        // A name that collided on merge names more than one bone here; the first is the one the
        // rig itself resolves a bare lookup to, so a mesh's own table has to agree with it.
        let mut named: HashMap<String, usize> = HashMap::new();
        for (bone, name) in rig.names().iter().enumerate() {
            named.entry(name.clone()).or_insert(bone);
        }
        let anchor = named.get(ANCHOR).copied();
        let (home, spread) = middle(&world, anchor);
        let seats = rig
            .names()
            .iter()
            .enumerate()
            .filter(|(_, name)| {
                name.strip_prefix(SEAT)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
            })
            .map(|(bone, _)| bone)
            .collect();
        Self {
            rig,
            rest,
            named,
            anchor,
            home,
            spread,
            seats,
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
    /// Every bone's own placement this frame, in the model's own space, for whatever wants a joint
    /// on its own rather than through a mesh's palette. Empty until the rig has landed.
    pub world: Vec<Mat4>,
    /// Where this rig seats a rider, for the one that is a mount.
    seat: Option<Placement>,
}

/// What a pack names the motion a model stands in, whatever rig it is built on.
const IDLE: &str = "_id0";

/// The seated idle a mount's own per-seat pack names. Picked by exact name rather than by
/// [`Motions::standing`]'s suffix guess: the pack also carries an additive breathing layer and,
/// for the seat a driver takes, a mount-up transition, and nothing about their own names rules
/// them out as reliably as this one being named for what it is.
const RIDE_IDLE: &str = "cbnm_mt_id0";

/// The `cfxf_` clip a motion's own timeline plays over it, and where in the motion's own clock
/// (in seconds) it is held across: a `C010` states this as a frame count and a start/end fraction
/// of the clip itself, which only means anything once scaled by the timeline it is read from.
#[derive(Clone)]
struct Companion {
    name: String,
    /// Seconds into the motion's own clock the hold starts and ends.
    window: (f32, f32),
    /// The clip's own position, normalized start to end, that the window plays across.
    span: (f32, f32),
}

/// The motions a pack holds, and the name each of its animations gives one.
struct Motions {
    /// Animation names, each with the motion it plays.
    named: Vec<(String, usize)>,
    /// The companion each of `named`'s own timeline plays over it, parallel to it. An emote often
    /// states its facial expression this way rather than by a name the creator picks.
    companions: Vec<Option<Companion>>,
    bindings: Vec<Binding>,
}

impl Motions {
    fn read(bytes: &[u8]) -> Result<Self> {
        let file = AnimationPack::read(Cursor::new(bytes.to_vec()))?;
        let bindings = file.parse_animations()?;
        let (mut named, mut companions) = (Vec::new(), Vec::new());
        for (animation, timeline) in file.animations().iter().zip(file.timelines()) {
            let Some(motion) = usize::try_from(animation.havok_index())
                .ok()
                .filter(|motion| bindings.get(*motion).is_some())
            else {
                continue;
            };
            named.push((animation.name().to_owned(), motion));
            let duration = bindings[motion].motion().duration();
            companions.push(companion(timeline, duration));
        }
        Ok(Self {
            named,
            companions,
            bindings,
        })
    }

    /// The motion the picker is on.
    fn binding(&self, motion: usize) -> Option<&Binding> {
        let (_, at) = self.named.get(motion)?;
        self.bindings.get(*at)
    }

    /// The companion the motion at `motion` names, if its own timeline names one.
    fn companion(&self, motion: usize) -> Option<&Companion> {
        self.companions.get(motion)?.as_ref()
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

/// The companion a motion's own timeline plays over it, out of the first `C009`/`C010` that plays
/// one: `duration` is the motion's own, in seconds, which is what the timeline's frame units are
/// scaled against. A timeline naming more than one, the way a longer emote's does to run through
/// several expressions in turn, is read as only the first; nothing here schedules a change of
/// companion mid-motion.
fn companion(timeline: &[u8], duration: f32) -> Option<Companion> {
    let timeline = Timeline::read(Cursor::new(timeline.to_vec())).ok()?;
    let frames = timeline.items().iter().find_map(|item| match item {
        Item::Header(header) => Some(header.duration()),
        _ => None,
    })?;
    if frames <= 0 {
        return None;
    }
    let scale = duration / f32::from(frames);
    timeline.items().iter().find_map(|item| {
        let Item::Command(command) = item else {
            return None;
        };
        let (path, hold, span) = match command.kind() {
            CommandKind::C009(animation) => (animation.path(), animation.duration(), (0.0, 1.0)),
            CommandKind::C010(animation) => (
                animation.path(),
                animation.duration(),
                // `0x01` enables the start and end frames; without it the whole clip plays.
                match animation.flags() & 0x01 != 0 {
                    true => (animation.animation_start(), animation.animation_end()),
                    false => (0.0, 1.0),
                },
            ),
            _ => return None,
        };
        let name = path?.strip_prefix("cfxf_")?.to_owned();
        let start = f32::from(command.time()) * scale;
        Some(Companion {
            name,
            window: (start, start + hold as f32 * scale),
            span,
        })
    })
}

/// A clip on its way out from under whatever replaced it, kept whole so it can go on being
/// sampled across the fade.
struct Leaving {
    path: String,
    pack: Rc<Motions>,
    motion: usize,
    time: f32,
}

/// One motion playing on the rig: the pack it comes from, which of that pack's motions, and how
/// far into it.
#[derive(Default)]
struct Layer {
    /// The pack to play, as the user or an emote has it.
    wanted: RefCell<String>,
    pack: RefCell<Option<Fetch<Rc<Motions>>>>,
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
    /// Paths still to try, in order, if the one loading now lands without `opening`'s motion. A
    /// name and its file disagree often enough that a guess has to be verified rather than
    /// trusted on sight.
    retry: RefCell<Vec<String>>,
    /// The clip the last change is fading out of, sampled under the incoming one until the fade
    /// closes. A layer with nothing wanted fades this one out to nothing instead, which is what
    /// lets the layers under it back through.
    leaving: RefCell<Option<Leaving>>,
    /// How far into the fade, in seconds, and how long it runs. No length is a hard cut.
    fade: Cell<f32>,
    over: Cell<f32>,
    /// How long to fade this layer out over once the clip has played through with nothing queued
    /// behind it. None means it loops, which is what a pose held forever wants.
    settle: Cell<f32>,
}

impl Layer {
    fn load(&self, path: &str, motion: Option<&str>, then: Option<&str>, fade: f32) {
        *self.leaving.borrow_mut() = (fade > 0.0).then(|| self.leaving_clip()).flatten();
        self.fade.set(0.0);
        self.over.set(fade);
        self.settle.set(0.0);
        path.clone_into(&mut self.wanted.borrow_mut());
        *self.pack.borrow_mut() = None;
        *self.then.borrow_mut() = then.map(ToOwned::to_owned);
        *self.opening.borrow_mut() = motion.map(ToOwned::to_owned);
        *self.retry.borrow_mut() = Vec::new();
        self.motion.set(None);
        self.time.set(0.0);
    }

    /// Plays `path` through once and then fades the layer back out over `fade`, which is what an
    /// action laid over a base pose does rather than loop on top of it forever.
    fn once(&self, path: &str, motion: Option<&str>, fade: f32) {
        self.load(path, motion, None, fade);
        self.settle.set(fade);
    }

    /// What is playing now, ready to go on being sampled after the layer has moved off it.
    fn leaving_clip(&self) -> Option<Leaving> {
        let pack = self.pack.borrow();
        Some(Leaving {
            path: self.wanted.borrow().clone(),
            pack: Rc::clone(pack.as_ref().and_then(Fetch::ready)?),
            motion: self.motion.get()?,
            time: self.time.get(),
        })
    }

    /// How much of the incoming clip shows: none until the fade opens, all of it once it has
    /// closed. A layer with nothing wanted reads this as how far its outgoing clip has faded out.
    fn share(&self) -> f32 {
        match self.over.get() > 0.0 {
            true => (self.fade.get() / self.over.get()).clamp(0.0, 1.0),
            false => 1.0,
        }
    }

    /// Loads the first of `candidates` opening on `motion`, keeping the rest to try in turn if it
    /// lands without that motion. An empty list leaves the layer at rest.
    fn seek(&self, mut candidates: Vec<String>, motion: &str, fade: f32) {
        match candidates.is_empty() {
            true => self.load("", None, None, fade),
            false => {
                let first = candidates.remove(0);
                self.load(&first, Some(motion), None, fade);
                *self.retry.borrow_mut() = candidates;
            }
        }
    }

    /// Whether the layer has run out of candidates without landing on `opening`'s motion,
    /// however it got there: nothing was ever wanted, or every candidate `seek` was given has
    /// landed and none of them named it.
    fn spent(&self) -> bool {
        if self.wanted.borrow().is_empty() {
            return true;
        }
        let landed = matches!(
            self.pack.borrow().as_ref(),
            Some(Fetch::Ready(_)) | Some(Fetch::Failed(_))
        );
        landed && self.retry.borrow().is_empty() && self.motion.get().is_none()
    }

    /// Takes up the pack once it lands, opening on the motion asked for. A pack that lands
    /// without it, or never arrives at all, gives way to the next candidate `seek` queued, or
    /// what was queued behind it once that runs out too: not every race ships the motion an
    /// emote starts with, and a file's name is not always its content.
    fn poll(&self, backend: &Backend) {
        let wanted = self.wanted.borrow().clone();
        let mut held = self.pack.borrow_mut();
        if wanted.is_empty() || !matches!(held.as_ref(), None | Some(Fetch::Fetching(_))) {
            return;
        }
        Fetch::poll(&mut held, backend, &wanted, |bytes| {
            Motions::read(bytes).map(Rc::new)
        });
        let ready = held.as_ref().and_then(Fetch::ready);
        let opening = self.opening.borrow().clone();
        let motion = ready.and_then(|motions| match opening.as_deref() {
            Some(name) => motions.named.iter().position(|(held, _)| held == name),
            None => motions.standing(),
        });
        let failed = matches!(held.as_ref(), Some(Fetch::Failed(_)));
        let missed = opening.is_some() && ready.is_some() && motion.is_none();
        drop(held);
        if failed || missed {
            let next = {
                let mut retry = self.retry.borrow_mut();
                (!retry.is_empty()).then(|| retry.remove(0))
            };
            if let Some(next) = next {
                next.clone_into(&mut self.wanted.borrow_mut());
                *self.pack.borrow_mut() = None;
                self.motion.set(None);
                self.time.set(0.0);
                return;
            }
        }
        self.motion.set(motion);
        if failed {
            let then = self.then.borrow_mut().take();
            if let Some(then) = then {
                self.load(&then, None, None, self.over.get());
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

    /// The companion the motion now playing names, if its own timeline names one.
    fn companion(&self) -> Option<Companion> {
        let pack = self.pack.borrow();
        let motions = pack.as_ref().and_then(Fetch::ready)?;
        motions.companion(self.motion.get()?).cloned()
    }

    /// The pack playing, the name its own file gives the motion, and how far into it, in
    /// seconds: for whatever reads a motion's timeline directly rather than through the pose it
    /// drives.
    fn playing(&self) -> Option<(String, String, f32)> {
        let pack = self.pack.borrow();
        let motions = pack.as_ref().and_then(Fetch::ready)?;
        let (name, _) = motions.named.get(self.motion.get()?)?;
        Some((self.wanted.borrow().clone(), name.clone(), self.time.get()))
    }

    /// Runs the clock on by `step`, taking up whatever was queued behind the motion once it has
    /// played through. Nothing queued means it loops, unless the clip was played once, in which
    /// case the layer fades back out from under itself.
    fn advance(&self, step: f32) {
        self.fading(step);
        let Some(duration) = self.duration() else {
            return;
        };
        let time = self.time.get() + step.min(duration);
        if time <= duration {
            self.time.set(time);
            return;
        }
        let then = self.then.borrow_mut().take();
        let settle = self.settle.get();
        match (then, settle > 0.0) {
            (Some(then), _) => self.load(&then, None, None, self.over.get()),
            (None, true) => self.load("", None, None, settle),
            (None, false) => self.time.set(time - duration),
        }
    }

    /// Runs the fade on by `step`, and the outgoing clip's own clock with it. The fade only opens
    /// once the incoming pack has landed, since a clip that is still being fetched has nothing to
    /// fade towards; a layer with nothing wanted is fading out to whatever is under it and opens
    /// straight away.
    fn fading(&self, step: f32) {
        let mut leaving = self.leaving.borrow_mut();
        let Some(held) = leaving.as_mut() else {
            return;
        };
        if self.motion.get().is_none() && !self.wanted.borrow().is_empty() {
            return;
        }
        let duration = held
            .pack
            .binding(held.motion)
            .map_or(f32::EPSILON, |binding| {
                binding.motion().duration().max(f32::EPSILON)
            });
        held.time = (held.time + step.min(duration)) % duration;
        self.fade.set(self.fade.get() + step);
        if self.share() >= 1.0 {
            *leaving = None;
        }
    }

    /// Sets this layer's clock from `at`, the other layer's own, against the window `companion`
    /// states, rather than running one of its own: a facial clip a fraction of a second long
    /// otherwise loops many times over while the body it belongs to plays once.
    fn hold(&self, companion: &Companion, at: f32) {
        let Some(duration) = self.duration() else {
            return;
        };
        self.time.set(held(companion, at, duration));
    }
}

/// Where a `duration`-second clip should sit to hold `companion` against `at`, the other clip's
/// own time in seconds: clamped, so it settles at the window's own edge rather than snap back to
/// it before the window opens or past it once it has closed.
fn held(companion: &Companion, at: f32, duration: f32) -> f32 {
    let (start, end) = companion.window;
    let fraction = ((at - start) / (end - start).max(f32::EPSILON)).clamp(0.0, 1.0);
    let (from, to) = companion.span;
    (from + (to - from) * fraction) * duration
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

/// The `cfxf_` names a pap's own animation table states, regardless of whether its own timeline
/// also names a companion.
fn pose_names(bytes: &[u8]) -> Result<Vec<String>> {
    let file = AnimationPack::read(Cursor::new(bytes.to_vec()))?;
    Ok(file
        .animations()
        .iter()
        .filter_map(|animation| animation.name().strip_prefix("cfxf_").map(ToOwned::to_owned))
        .collect())
}

/// What a lookup against [`Poses`] found.
enum PoseLookup {
    /// The index is still being built; ask again once more of it has landed.
    Pending,
    Found(String),
    /// The index finished without ever seeing the name.
    Miss,
}

/// Every `.pap` under a face's own tree, read one at a time and kept for the rest of the
/// session. Built only once a pose neither the filename guess nor the shared library could
/// confirm asks for it, since walking the whole tree costs hundreds of fetches a face rarely
/// needs.
#[derive(Default)]
enum Poses {
    #[default]
    Unbuilt,
    Building {
        queue: Vec<String>,
        current: Option<String>,
        fetch: Option<Fetch<Vec<String>>>,
        found: HashMap<String, String>,
    },
    Ready(HashMap<String, String>),
}

impl Poses {
    /// Looks `name` up, advancing the walk by one fetch if it has not been seen yet. `paths` is
    /// only read on the first call, to seed the walk from the listing already fetched.
    fn advance(&mut self, backend: &Backend, paths: Vec<String>, name: &str) -> PoseLookup {
        if matches!(self, Self::Unbuilt) {
            let mut queue = paths;
            queue.reverse();
            *self = Self::Building {
                queue,
                current: None,
                fetch: None,
                found: HashMap::new(),
            };
        }
        loop {
            match self {
                Self::Ready(found) => {
                    return match found.get(name) {
                        Some(path) => PoseLookup::Found(path.clone()),
                        None => PoseLookup::Miss,
                    };
                }
                Self::Building {
                    queue,
                    current,
                    fetch,
                    found,
                } => {
                    if let Some(path) = found.get(name) {
                        return PoseLookup::Found(path.clone());
                    }
                    let path = match current.clone() {
                        Some(path) => path,
                        None => match queue.pop() {
                            Some(next) => {
                                *current = Some(next.clone());
                                next
                            }
                            None => {
                                let found = std::mem::take(found);
                                *self = Self::Ready(found);
                                continue;
                            }
                        },
                    };
                    Fetch::poll(fetch, backend, &path, pose_names);
                    match fetch.take() {
                        Some(Fetch::Ready(names)) => {
                            for pose in names {
                                found.entry(pose).or_insert_with(|| path.clone());
                            }
                            *current = None;
                        }
                        Some(Fetch::Failed(_)) => *current = None,
                        other => {
                            *fetch = other;
                            return PoseLookup::Pending;
                        }
                    }
                }
                Self::Unbuilt => unreachable!(),
            }
        }
    }
}

/// One pack a model can be played from, as the listing names it.
struct Pack {
    path: String,
    /// What the picker calls it: the path with everything every pack shares cut off the front.
    label: String,
}

/// A rig's own bones, each one's parent, and the matrix that carries a bind-pose vertex into that
/// bone's own rest frame.
pub type RigInfo = (Vec<String>, Vec<Option<usize>>, Vec<Mat4>);

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
    /// What the body does, the one-shot laid over it, and the expression over that. A facial
    /// motion states a delta on bones the body's own motions never touch, so the two play at once
    /// rather than in turn; an action is a partial motion that owns the bones it names for as long
    /// as it runs and gives them back to the base once it has.
    body: Layer,
    action: Layer,
    face: Layer,
    /// The `cfxf_` companion last used to drive `face` on the body's own say-so, so a change of it
    /// is what asks for another rather than every frame re-loading the same pack.
    linked: RefCell<Option<String>>,
    /// Whether `face` is still the pack `linked` put it on, so its clock tracks the body's own
    /// rather than running free. `express` and a manual face pick from the picker both drop this,
    /// since a pose the creator asked for by name is not the body's to hold or let go of.
    synced: Cell<bool>,
    /// A pose `express` or the body's own companion asked for that neither the filename guess
    /// nor the shared library could confirm, waiting on `poses` to be built far enough to answer.
    pending: RefCell<Option<String>>,
    /// Every `.pap` under the face's own tree, read one at a time and kept for the session: the
    /// last resort once a pose's name and its likely files disagree.
    poses: RefCell<Poses>,
    /// What the bust bones are scaled by, three axes in their own frame.
    bust: Cell<Vec3>,
    /// How far a raised visor has turned, one angle per bone it hinges on.
    visor: Cell<[f32; 3]>,
    running: Cell<bool>,
    /// The mount the body is seated on, posed on a rig of its own. A mount names the same bones a
    /// body does, so the two cannot be merged the way an extra skeleton is.
    mounted: Option<Box<Animation>>,
    /// Which seat this rig is in: on a mount, the bone a rider is carried to; on a rider, which
    /// of the mount's own per-seat packs it plays. A mount that seats more than one names a pose
    /// of its own for each.
    seat: Cell<usize>,
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
                // A guess to stand in until the listing lands and `listed` picks properly: the
                // mount's own seat 0, or the plain idle where there is no mount to guess a code
                // for. Never the whistle a mount is called with; that is not a pose to hold.
                wanted: RefCell::new(
                    code.as_deref()
                        .and_then(|code| match &mount {
                            Some(mount) => seat_path(code, mount, 0).or_else(|| pack_path(code)),
                            None => pack_path(code),
                        })
                        .unwrap_or_default(),
                ),
                ..Default::default()
            },
            action: Default::default(),
            face: Default::default(),
            linked: RefCell::new(None),
            synced: Cell::new(false),
            pending: RefCell::new(None),
            poses: Default::default(),
            bust: Cell::new(Vec3::ONE),
            visor: Cell::new([0.0; 3]),
            running: Cell::new(true),
            mounted: mount.map(|mount| Box::new(Animation::new(filed_under(&mount, &models)))),
            seat: Cell::new(0),
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

    /// The rig everything is posed on, once it has landed: its bones, each one's parent, and the
    /// matrix that carries a bind-pose vertex into that bone's own rest frame.
    pub fn rig(&self) -> Option<RigInfo> {
        let skin = self.skin.borrow();
        let skin = skin.as_ref()?;
        let parents = (0..skin.rig.bones())
            .map(|bone| skin.rig.parent(bone))
            .collect();
        Some((skin.rig.names().to_vec(), parents, skin.rest.clone()))
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
        self.poll_companion();
        self.poll_pose(backend);
        if self.running.get() {
            let step = ctx.input(|input| input.stable_dt);
            self.body.advance(step);
            self.action.advance(step);
            // A body command names a window of its own clock to hold the face against rather than
            // let it loop on one of its own; nothing named, or a face the creator has since picked
            // by hand, leaves it free to run on its own clock instead.
            match self.body.companion() {
                Some(companion) if self.synced.get() => {
                    self.face.hold(&companion, self.body.time.get());
                }
                _ => self.face.advance(step),
            }
            // Nothing else asks for a frame while the pointer is still, so playback has to.
            ctx.request_repaint();
        }
    }

    /// The layers in the order they are laid: the base first, then whatever owns the bones it
    /// names over it, then the face over that.
    fn layers(&self) -> [&Layer; 3] {
        [&self.body, &self.action, &self.face]
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

    /// Every pack the lineage this body is built on files, nearest first, opened on the nearest
    /// one's own idle (or its ride pack, mounted). A race rarely authors every motion its own
    /// body plays: `battle_dead_1` ships only under `c0101`, so a Lalafell's own directory alone
    /// would never offer it, and every other body's list is unioned in rather than replaced by
    /// the first non-empty one. Where two bodies both ship a pack of the same name, the nearer's
    /// is kept.
    fn listed(&self, listing: &Listing) -> Vec<Pack> {
        let mut listed: Vec<Pack> = Vec::new();
        let mut named: HashSet<String> = HashSet::new();
        for code in self.built_on.borrow().iter() {
            let Some(root) = pack_root(code) else {
                continue;
            };
            for pack in found(&root, listing.under(&root)) {
                if named.insert(pack.label.clone()) {
                    listed.push(pack);
                }
            }
        }
        listed.sort_by(|left, right| left.label.cmp(&right.label));
        let idle = match self
            .mounted
            .as_ref()
            .and_then(|mounted| mounted.code.as_deref())
        {
            Some(mount) => self.ride_pack(mount, self.seat.get(), &listed),
            None => {
                let exists = |path: Option<String>| {
                    path.filter(|path| listed.iter().any(|pack| pack.path == *path))
                };
                self.built_on
                    .borrow()
                    .iter()
                    .find_map(|code| exists(pack_path(code)))
                    .map(|path| (path, None))
            }
        };
        // The placeholder set at construction is only ever a guess, so the conventional pack
        // always overrides it once the listing is in; a weapon is named none at all, and only
        // then does the listing's own first pack stand in.
        if let Some((path, motion)) =
            idle.or_else(|| listed.first().map(|pack| (pack.path.clone(), None)))
        {
            self.body.load(&path, motion, None, 0.0);
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
        for path in &landed {
            let Some(held) = extras[path].as_ref().and_then(Fetch::ready) else {
                continue;
            };
            rig = rig.merged(path, &held.names, &held.parents, &held.reference);
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

    /// How far a raised visor has turned, in radians, one angle per bone it hinges on.
    pub fn hinged(&self, visor: [f32; 3]) {
        self.visor.set(visor);
    }

    /// Which of the mount's own seats the rider takes, for the one that is a mount seating more
    /// than one. A body that is not riding has nowhere to put this. A change of seat asks for the
    /// pose that seat plays rather than waiting for the pack list to notice on its own.
    pub fn seated(&self, seat: usize) {
        if let Some(mounted) = &self.mounted {
            mounted.seat.set(seat);
        }
        let Some(mount) = self
            .mounted
            .as_ref()
            .and_then(|mounted| mounted.code.as_deref())
        else {
            return;
        };
        if self.seat.replace(seat) == seat {
            return;
        }
        let packs = self.packs.borrow();
        if let Some(packs) = packs.as_ref().and_then(|packs| packs.as_ref().ok())
            && let Some((path, motion)) = self.ride_pack(mount, seat, packs)
        {
            self.body.load(&path, motion, None, 0.0);
        }
    }

    /// The pose a mount's own seat plays, out of the packs given: its own, by exact name, where
    /// the mount ships one, else the plain standing idle every body has. Neither is the whistle a
    /// mount is called with, which holds no seated pose at all.
    fn ride_pack(
        &self,
        mount: &str,
        seat: usize,
        packs: &[Pack],
    ) -> Option<(String, Option<&'static str>)> {
        let exists =
            |path: Option<String>| path.filter(|path| packs.iter().any(|pack| pack.path == *path));
        self.built_on
            .borrow()
            .iter()
            .find_map(|code| exists(seat_path(code, mount, seat)))
            .map(|path| (path, Some(RIDE_IDLE)))
            .or_else(|| {
                self.built_on
                    .borrow()
                    .iter()
                    .find_map(|code| exists(pack_path(code)))
                    .map(|path| (path, None))
            })
    }

    /// The pack, motion name and time the body is playing, for an emote's own timeline commands
    /// (props, sound, vfx) rather than the face's: those are read against whatever the body is
    /// doing, not the expression laid over it.
    pub fn body_playing(&self) -> Option<(String, String, f32)> {
        self.body.playing()
    }

    /// Plays `path`, settling into `then` once it has played through, cross-fading out of whatever
    /// was playing over `fade` seconds.
    ///
    /// A pack of facial motions plays over whatever the body is doing rather than in place of it,
    /// so which of the two it lands on is the pack's to say.
    pub fn play(&self, path: &str, then: Option<&str>, fade: f32) {
        if facial(path) {
            self.synced.set(false);
            self.face.load(path, None, then, fade);
        } else {
            self.body.load(path, None, then, fade);
        }
        // Forces the next poll to re-read the companion rather than see the same name it had
        // last time and assume nothing changed, which is what left a re-picked emote's face
        // stuck on whatever frame it was already at.
        *self.linked.borrow_mut() = None;
        self.running.set(true);
    }

    /// Lays `motion` from `path` over whatever the body is doing for as long as it runs, fading in
    /// and back out over `fade` seconds. A partial motion names only the bones it moves, so the
    /// base keeps every other one for the whole of it.
    pub fn act(&self, path: &str, motion: &str, fade: f32) {
        self.action.once(path, Some(motion), fade);
        self.running.set(true);
    }

    /// Puts an expression on the face the character wears. A file's own name is only a guess at
    /// what it holds, so every candidate is opened on the `cfxf_` name itself and skipped if it
    /// does not carry it: the filename match first, since most poses are a pack of their own,
    /// then the one a face keeps resident, then the rest of the face's own tree if neither knew
    /// it. A name filed nowhere leaves the face as it rests, which is the game's own neutral pose.
    pub fn express(&self, name: &str) {
        let Some(root) = self.face_root() else {
            return;
        };
        let file = format!("{name}.pap");
        let mut candidates: Vec<String> = match self.packs.borrow().as_ref() {
            Some(Ok(packs)) => packs
                .iter()
                .filter(|pack| pack.path.starts_with(&root) && file_name(&pack.path) == file)
                .map(|pack| pack.path.clone())
                .collect(),
            _ => Vec::new(),
        };
        candidates.push(format!("{root}resident/face.pap"));
        self.face.seek(candidates, &format!("cfxf_{name}"), 0.0);
        *self.pending.borrow_mut() = Some(name.to_owned());
        self.synced.set(false);
        self.running.set(true);
    }

    /// Drives the face from the `cfxf_` companion the body's own motion names, the way an emote
    /// like Joy carries its own expression rather than leaving the creator to pick one. A change of
    /// companion is what asks for another; a body pack with none resets the face to rest rather
    /// than leaving it holding whatever it played last.
    fn poll_companion(&self) {
        let wanted = self.body.companion();
        let name = wanted.as_ref().map(|companion| companion.name.clone());
        if name == *self.linked.borrow() {
            return;
        }
        let Some(companion) = &wanted else {
            *self.linked.borrow_mut() = name;
            self.synced.set(false);
            self.face.load("", None, None, 0.0);
            return;
        };
        let name = &companion.name;
        let Some(root) = self.face_root() else {
            return;
        };
        let held = self.packs.borrow();
        let Some(Ok(packs)) = held.as_ref() else {
            return;
        };
        let tail = file_name(&self.body.wanted.borrow())
            .strip_suffix(".pap")
            .unwrap_or_default()
            .to_owned();
        let candidates: Vec<String> = [
            format!("{root}nonresident/{name}.pap"),
            format!("{root}nonresident/emot/{tail}.pap"),
            format!("{root}resident/face.pap"),
        ]
        .into_iter()
        .filter(|candidate| packs.iter().any(|pack| pack.path == *candidate))
        .collect();
        drop(held);
        self.face.seek(candidates, &format!("cfxf_{name}"), 0.0);
        *self.pending.borrow_mut() = Some(name.clone());
        *self.linked.borrow_mut() = Some(name.clone());
        self.synced.set(true);
    }

    /// Falls back to the lazily-built name index for a pose `express` or `poll_companion` could
    /// not confirm any faster way, once the face layer has run out of candidates to try on its
    /// own.
    fn poll_pose(&self, backend: &Backend) {
        let Some(name) = self.pending.borrow().clone() else {
            return;
        };
        if self.face.motion.get().is_some() {
            *self.pending.borrow_mut() = None;
            return;
        }
        if !self.face.spent() {
            return;
        }
        let Some(root) = self.face_root() else {
            *self.pending.borrow_mut() = None;
            return;
        };
        let held = self.packs.borrow();
        let Some(Ok(packs)) = held.as_ref() else {
            return;
        };
        let paths: Vec<String> = packs
            .iter()
            .filter(|pack| pack.path.starts_with(&root) && pack.path.ends_with(".pap"))
            .map(|pack| pack.path.clone())
            .collect();
        drop(held);
        match self.poses.borrow_mut().advance(backend, paths, &name) {
            PoseLookup::Pending => {}
            PoseLookup::Found(path) => {
                self.face.load(&path, Some(&format!("cfxf_{name}")), None, 0.0);
                *self.pending.borrow_mut() = None;
            }
            PoseLookup::Miss => *self.pending.borrow_mut() = None,
        }
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
        let mut lay = |path: &str, binding: &Binding, time: f32, weight: f32| {
            let ordered = self.code.as_deref().and_then(|code| ordering(code, path));
            let held = match &ordered {
                Some(path) => extras.get(path).and_then(Option::as_ref),
                None => base.as_ref(),
            };
            let Some(names) = held.and_then(Fetch::ready).map(|held| &held.names) else {
                return;
            };
            skin.rig
                .lay(&mut locals, binding, names, ordered.as_deref(), time, weight);
        };
        for layer in self.layers() {
            let share = layer.share();
            if let Some(leaving) = layer.leaving.borrow().as_ref()
                && let Some(binding) = leaving.pack.binding(leaving.motion)
            {
                // Nothing wanted means the layer is on its way out from over the ones under it, so
                // what is left of the clip it was playing is all there is to lay.
                let weight = match layer.wanted.borrow().is_empty() {
                    true => 1.0 - share,
                    false => 1.0,
                };
                lay(&leaving.path, binding, leaving.time, weight);
            }
            let pack = layer.pack.borrow();
            let Some(binding) = layer
                .motion
                .get()
                .and_then(|motion| pack.as_ref().and_then(Fetch::ready)?.binding(motion))
            else {
                continue;
            };
            lay(
                &layer.wanted.borrow(),
                binding,
                layer.time.get(),
                match layer.leaving.borrow().is_some() {
                    true => share,
                    false => 1.0,
                },
            );
        }
        for (name, angle) in VISOR.iter().zip(self.visor.get()) {
            if angle != 0.0
                && let Some(bone) = skin.named.get(*name)
                && let Some(local) = locals.get_mut(*bone)
            {
                let turned = Quat::from_array(local.rotation) * Quat::from_rotation_z(angle);
                local.rotation = turned.to_array();
            }
        }
        let mut posed = skin.rig.world(&locals);
        let bust = self.bust.get();
        if bust != Vec3::ONE {
            for bone in BUST.iter().filter_map(|name| skin.named.get(*name)) {
                posed[*bone] = posed[*bone].scaled(bust);
            }
        }
        let (center, spread) = middle(&posed, skin.anchor);
        // A seat past what this rig's own skeleton names is a vehicle-class mount whose extra
        // riders have no bone of their own; falling back to the first keeps them on the mount at
        // all rather than carrying nothing.
        let seat = skin
            .seats
            .get(self.seat.get())
            .or_else(|| skin.seats.first())
            .map(|bone| posed[*bone]);
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
            world: posed.iter().map(Placement::matrix).collect(),
            seat,
        }
    }

    /// Which packs are loaded, which motion each of them plays, play and pause, and the scrubber.
    /// Only the pickers are offered until a motion is picked: with none the model stands where its
    /// own file put it, and there is nothing to play.
    pub fn ui(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| self.row(ui));
        if let Some(mounted) = &self.mounted {
            ui.horizontal_wrapped(|ui| {
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
            self.play(&path, None, 0.0);
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
            self.body.load(&wanted, None, None, 0.0);
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

/// The pack a mount names for one of its own seats, 1-based: a two-seater's driver leans and sits
/// differently from its passenger, and a bench seating several turns some of them toward the one
/// driving rather than facing forward, so each seat's pose is filed apart from the others rather
/// than shared. Most mounts ship none of these and fall back to the plain standing idle
/// [`pack_path`] already names: `bt_common/mount/mount_start.pap` is not a pose to fall back to at
/// all, whatever seat is asked for, since its one motion is the whistle a mount is called with.
fn seat_path(code: &str, mount: &str, seat: usize) -> Option<String> {
    Some(format!(
        "chara/{}/{code}/animation/a0001/mt_{mount}/resident/mount{:02}.pap",
        tree(code)?,
        seat + 1
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
    use std::rc::Rc;

    use glam::{Mat4, Vec3};
    use ironworks::file::sklb::Transform;

    use super::super::super::skeleton::{Rig, middle};
    use super::{
        Animation, Companion, Extra, Fetch, Layer, Leaving, Motions, PoseLookup, Poses, Skeleton,
        Skin, code, extra, facial, found, held, ordering, pack_path, pack_root, seat_path,
        skeleton_path,
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

    /// A mount's seats are `n_mount` and whatever else the skeleton names after it, in the order
    /// it lists them; a name that only starts the same, like a decorative `n_mounted_light`, is
    /// not a seat.
    #[test]
    fn a_mount_names_its_seats_in_skeleton_order() {
        let names = [
            "n_root",
            "n_mount",
            "n_mount_a",
            "n_mounted_light",
            "n_mount_b",
        ]
        .map(ToOwned::to_owned);
        let reference: Vec<_> = names.iter().map(|_| transform([0.0, 0.0, 0.0])).collect();
        let rig = Rig::new(&names, &[-1, 0, 0, 0, 0], &reference);
        let skin = Skin::new(rig);
        assert_eq!(skin.seats, [1, 2, 4]);
    }

    /// A face is skinned to bones the body's own skeleton has never heard of, and its own skeleton
    /// hangs them off one the body does name.
    #[test]
    fn a_face_bone_is_posed_once_its_own_skeleton_is_merged_in() {
        let base = rig();
        assert_eq!(base.bones(), 2);
        let merged = base.merged(
            "face",
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

    /// A mesh's own table still means the body's bone by a name a merge had to keep apart from an
    /// extra's: `Skin`'s own lookup has to agree with `Rig::bone`'s, or a mesh skinned to the
    /// base's `j_ago` would draw off the face's instead of the body's the moment one collided.
    #[test]
    fn a_meshs_bare_lookup_still_means_the_bases_own_bone() {
        let base = rig();
        let merged = base.merged(
            "face",
            &["j_kubi".to_owned(), "j_kubi".to_owned()],
            &[-1, 0],
            &[transform([0.0, 9.0, 0.0]), transform([0.0, 0.5, 0.0])],
        );
        assert_eq!(merged.bones(), 3);
        let base_kubi = merged.bone("j_kubi").expect("the body keeps its own");
        let skin = Skin::new(merged);
        assert_eq!(skin.named["j_kubi"], base_kubi);
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
        assert_eq!(
            seat_path("c0101", "m0547", 3).as_deref(),
            Some("chara/human/c0101/animation/a0001/mt_m0547/resident/mount04.pap")
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

    #[test]
    fn seek_queues_the_rest_as_retries() {
        let layer = Layer::default();
        layer.seek(vec!["a.pap".to_owned(), "b.pap".to_owned()], "cfxf_salute", 0.0);
        assert_eq!(*layer.wanted.borrow(), "a.pap");
        assert_eq!(*layer.retry.borrow(), vec!["b.pap".to_owned()]);
        assert_eq!(layer.opening.borrow().as_deref(), Some("cfxf_salute"));
    }

    #[test]
    fn seek_with_nothing_to_try_rests() {
        let layer = Layer::default();
        layer.seek(Vec::new(), "cfxf_salute", 0.0);
        assert!(layer.wanted.borrow().is_empty());
        assert!(layer.opening.borrow().is_none());
    }

    #[test]
    fn spent_waits_for_a_landing_with_nothing_left_to_try() {
        let layer = Layer::default();
        assert!(layer.spent(), "nothing wanted yet");
        layer.seek(vec!["a.pap".to_owned()], "cfxf_salute", 0.0);
        assert!(!layer.spent(), "still fetching, no candidates behind it");
        *layer.pack.borrow_mut() = Some(Fetch::Failed("boom".to_owned()));
        assert!(
            layer.spent(),
            "landed with nothing left to try and no motion found"
        );
    }

    #[test]
    fn spent_stays_false_while_a_retry_is_queued() {
        let layer = Layer::default();
        layer.seek(vec!["a.pap".to_owned(), "b.pap".to_owned()], "cfxf_salute", 0.0);
        *layer.pack.borrow_mut() = Some(Fetch::Failed("boom".to_owned()));
        assert!(!layer.spent(), "b.pap is still queued behind a.pap");
    }

    /// `c1801`'s own Joy emote, measured against the install: a `Header duration: 122` timeline
    /// naming `C010 { duration: 103, animation_start: 0.0, animation_end: 1.0 }` over a body clip
    /// 4.0 seconds long, held against a face clip 2.0 seconds long. Before the window the face
    /// sits at its own first frame, inside it the two clocks track each other, and past it the
    /// face holds its last rather than snapping back to loop on a clock of its own.
    #[test]
    fn held_tracks_the_bodys_clock_across_the_window_and_clamps_past_it() {
        let scale = 4.0 / 122.0;
        let companion = Companion {
            name: "satisfied".to_owned(),
            window: (0.0, 103.0 * scale),
            span: (0.0, 1.0),
        };
        assert_eq!(held(&companion, -1.0, 2.0), 0.0);
        assert!((held(&companion, 103.0 * scale * 0.5, 2.0) - 1.0).abs() < 1e-4);
        assert!((held(&companion, 103.0 * scale, 2.0) - 2.0).abs() < 1e-4);
        assert_eq!(held(&companion, 4.0, 2.0), 2.0);
    }

    /// A pack holding nothing, for the fade arithmetic, which never reads what is playing.
    fn empty_pack() -> Rc<Motions> {
        Rc::new(Motions {
            named: Vec::new(),
            companions: Vec::new(),
            bindings: Vec::new(),
        })
    }

    fn leaving(layer: &Layer) {
        *layer.leaving.borrow_mut() = Some(Leaving {
            path: "a.pap".to_owned(),
            pack: empty_pack(),
            motion: 0,
            time: 0.0,
        });
    }

    #[test]
    fn a_change_with_no_length_cuts_straight_to_the_new_clip() {
        let layer = Layer::default();
        layer.motion.set(Some(0));
        *layer.pack.borrow_mut() = Some(Fetch::Ready(empty_pack()));
        layer.load("b.pap", None, None, 0.0);
        assert!(layer.leaving.borrow().is_none());
        assert_eq!(layer.share(), 1.0);
    }

    #[test]
    fn a_fade_holds_shut_until_the_incoming_pack_lands() {
        let layer = Layer::default();
        leaving(&layer);
        layer.over.set(0.4);
        "b.pap".clone_into(&mut layer.wanted.borrow_mut());
        layer.advance(0.2);
        assert_eq!(layer.share(), 0.0, "nothing has landed to fade towards");
        layer.motion.set(Some(0));
        layer.advance(0.2);
        assert_eq!(layer.share(), 0.5);
        layer.advance(0.2);
        assert!(layer.leaving.borrow().is_none(), "the fade closed");
    }

    #[test]
    fn a_released_layer_fades_out_from_over_the_ones_under_it() {
        let layer = Layer::default();
        layer.motion.set(Some(0));
        *layer.pack.borrow_mut() = Some(Fetch::Ready(empty_pack()));
        "a.pap".clone_into(&mut layer.wanted.borrow_mut());
        layer.load("", None, None, 0.5);
        assert!(layer.wanted.borrow().is_empty());
        layer.advance(0.25);
        assert_eq!(layer.share(), 0.5, "half of the outgoing clip is left");
        layer.advance(0.25);
        assert!(layer.leaving.borrow().is_none());
    }

    /// Polls a future to completion on the current thread with no real waker, which is enough for
    /// the local install's own I/O: nothing here needs to run concurrently with anything else.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::task::Wake;
        struct NoopWaker;
        impl Wake for NoopWaker {
            fn wake(self: std::sync::Arc<Self>) {}
        }
        let waker = std::task::Waker::from(std::sync::Arc::new(NoopWaker));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(value) => return value,
                std::task::Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
    }

    const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

    fn local_backend() -> crate::backend::Backend {
        block_on(crate::backend::Backend::new(crate::settings::BackendConfig {
            api_url: "https://exd.camora.dev".to_owned(),
            location: crate::settings::InstallLocation::Sqpack(SQPACK.to_owned()),
            schema: crate::settings::SchemaLocation::Local("/home/asriel/Code/EXDSchema".to_owned()),
        }))
        .unwrap()
    }

    /// Drives a layer's own polling loop against a real backend until it lands on a motion, runs
    /// out of candidates, or the budget below runs out.
    fn settle(layer: &Layer, backend: &crate::backend::Backend) {
        let ctx = egui::Context::default();
        for _ in 0..500 {
            crate::utils::tick_promises(&ctx);
            layer.poll(backend);
            if layer.spent() || layer.motion.get().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// `salute.pap` really carries `cfxf_bow`, per `dump`ing the real file: exactly the case the
    /// filename-first bug got wrong. Seeking it first with `resident/face.pap` behind it, which
    /// does carry `cfxf_salute`, should miss the filename guess and land on the fallback instead.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_filename_guess_that_lands_on_the_wrong_pose_falls_back() {
        let backend = local_backend();
        let root = "chara/human/c0101/animation/f0206/";
        let layer = Layer::default();
        layer.seek(
            vec![
                format!("{root}nonresident/emot/salute.pap"),
                format!("{root}resident/face.pap"),
            ],
            "cfxf_salute",
            0.0,
        );
        settle(&layer, &backend);
        assert_eq!(
            *layer.wanted.borrow(),
            format!("{root}resident/face.pap"),
            "the wrong-named guess should have been abandoned"
        );
        assert!(
            layer.motion.get().is_some(),
            "the fallback names cfxf_salute and should have landed on it"
        );
    }

    /// `nonresident/comeon.pap` (not the `emot/` one) is self-consistent: its name really matches
    /// its own `cfxf_comeon`. The filename guess should be kept rather than spent on the fallback.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_filename_guess_that_matches_is_kept() {
        let backend = local_backend();
        let root = "chara/human/c0101/animation/f0206/";
        let layer = Layer::default();
        layer.seek(
            vec![
                format!("{root}nonresident/comeon.pap"),
                format!("{root}resident/face.pap"),
            ],
            "cfxf_comeon",
            0.0,
        );
        settle(&layer, &backend);
        assert_eq!(
            *layer.wanted.borrow(),
            format!("{root}nonresident/comeon.pap"),
            "the matching guess should never have been abandoned"
        );
        assert!(layer.motion.get().is_some());
        assert_eq!(
            layer.retry.borrow().len(),
            1,
            "resident/face.pap should still be queued, not yet fetched"
        );
    }

    /// The lazy index has to walk past files that do not carry the name it is after before it
    /// reaches the one that does, and has to come back with a clean miss for a name in none of
    /// them: `act_emot27` names `cfxf_emot_eeh`, which only `nonresident/eeh.pap` carries, while
    /// `loop_emot32_loop`'s `cfxf_lookback_l` is nowhere in the tree at all.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn the_pose_index_walks_past_misses_to_a_real_hit_and_reports_a_true_one() {
        let backend = local_backend();
        let root = "chara/human/c0101/animation/f0206/";
        let paths: Vec<String> = [
            "nonresident/angry.pap",
            "nonresident/bow.pap",
            "nonresident/eeh.pap",
            "nonresident/kiss.pap",
        ]
        .into_iter()
        .map(|tail| format!("{root}{tail}"))
        .collect();

        let mut poses = Poses::default();
        let found = loop {
            match poses.advance(&backend, paths.clone(), "emot_eeh") {
                PoseLookup::Found(path) => break path,
                PoseLookup::Miss => panic!("emot_eeh is really in nonresident/eeh.pap"),
                PoseLookup::Pending => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        };
        assert_eq!(found, format!("{root}nonresident/eeh.pap"));

        let mut poses = Poses::default();
        loop {
            match poses.advance(&backend, paths.clone(), "lookback_l") {
                PoseLookup::Found(path) => panic!("lookback_l should not exist, found in {path}"),
                PoseLookup::Miss => break,
                PoseLookup::Pending => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    }

    /// Drives the base skeleton fetch and the extra-skeleton merge until the rig lands.
    fn settle_rig(animation: &Animation, backend: &crate::backend::Backend) {
        let ctx = egui::Context::default();
        let Some(path) = animation.skeleton.clone() else {
            panic!("a human model always names its own base skeleton");
        };
        for _ in 0..2000 {
            crate::utils::tick_promises(&ctx);
            Fetch::poll(
                &mut animation.base.borrow_mut(),
                backend,
                &path,
                Skeleton::read,
            );
            animation.poll_extras(backend);
            if animation.rig().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the rig never landed");
    }

    /// Viera's own face skeleton (`c1801f0002`) carries a `j_ago` that is not the body's jaw: the
    /// real merge, off the real install, has to keep both rather than let the face's vanish onto
    /// whichever one the body already named.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_real_vieras_face_keeps_its_own_jaw_distinct_from_the_bodys() {
        let backend = local_backend();
        let animation = Animation::new([
            "chara/human/c1801/obj/body/b0001/model/c1801b0001_top.mdl",
            "chara/human/c1801/obj/face/f0002/model/c1801f0002_fac.mdl",
        ]);
        settle_rig(&animation, &backend);
        let (names, parents, _) = animation.rig().expect("settled above");
        let agos: Vec<usize> = names
            .iter()
            .enumerate()
            .filter(|(_, name)| *name == "j_ago")
            .map(|(bone, _)| bone)
            .collect();
        assert_eq!(
            agos.len(),
            2,
            "the body's own jaw and the face's own must both survive the merge, found {names:?}"
        );
        let kao = names
            .iter()
            .position(|name| name == "j_kao")
            .expect("j_kao merges in as the face's own root");
        assert!(
            agos.iter().any(|bone| parents[*bone] == Some(kao)),
            "the face's own j_ago hangs off j_kao, same as the real file states"
        );
    }

    /// `cbem_joy`, off the real install: `Motions::read` has to carry `cfxf_satisfied`'s window
    /// through in seconds, scaled off the timeline's own `Header duration: 122` against `duration:
    /// 103`, rather than the bare frame counts the file states.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_real_joy_emote_holds_its_face_across_the_bodys_own_window() {
        let backend = local_backend();
        let path = "chara/human/c1801/animation/a0001/bt_common/emote/joy.pap";
        let bytes =
            block_on(backend.files().read(path)).expect("joy.pap should read off the real install");
        let motions = Motions::read(&bytes).expect("a real animation pack should parse");
        let at = motions
            .named
            .iter()
            .position(|(name, _)| name == "cbem_joy")
            .expect("cbem_joy should be named");
        let companion = motions
            .companion(at)
            .expect("cbem_joy names a facial companion");
        assert_eq!(companion.name, "satisfied");
        assert_eq!(companion.window.0, 0.0);
        assert_eq!(companion.span, (0.0, 1.0));
        let body_duration = motions
            .binding(at)
            .expect("cbem_joy has a binding")
            .motion()
            .duration();
        let expected_end = 103.0 / 122.0 * body_duration;
        assert!(
            (companion.window.1 - expected_end).abs() < 1e-4,
            "window end {} should scale duration 103 against Header duration 122, expected {expected_end}",
            companion.window.1
        );
    }
}
