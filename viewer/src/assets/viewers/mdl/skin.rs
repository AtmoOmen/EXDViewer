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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;

use anyhow::Result;
use egui::{Color32, RichText};
use glam::{Mat4, Vec3};
use ironworks::file::File;
use ironworks::file::pap::{AnimationPack, Binding};
use ironworks::file::sklb::SkeletonBinary;

use super::super::skeleton::{Placement, Rig, middle};
use super::super::{link, placed, section};
use crate::backend::Backend;
use crate::data::listing::Listed;
use crate::settings::api_base;
use crate::utils::{TrackedPromise, file_name};

/// What the picker calls standing the model where its own file put it.
const REST: &str = "Reference pose";
/// How tall the pack list is allowed to get. A human carries thousands of them.
const PACK_LIST_HEIGHT: f32 = 240.0;

/// The bone a body hangs off, which is what a pose is centred on. A tail carries many bones a long
/// way out and swings them, and averaging every bone instead walks the frame around with it.
const ANCHOR: &str = "n_hara";

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
}

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
                // A motion that blends is a delta over whatever is already playing rather than a
                // pose of its own, and posing a model on one scatters it.
                (bindings.get(motion)?.blend_hint() == 0)
                    .then(|| (animation.name().to_owned(), motion))
            })
            .collect();
        Ok(Self { named, bindings })
    }

    /// The motion the picker is on.
    fn binding(&self, motion: usize) -> Option<&Binding> {
        let (_, at) = self.named.get(motion)?;
        self.bindings.get(*at)
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

/// What plays a model: the skeleton it is skinned to, a pack of motions, and the clock.
pub struct Animation {
    /// Where the model's own path says its skeleton is, and the rig that came of it.
    skeleton: Option<String>,
    skin: RefCell<Option<Fetch<Skin>>>,
    /// Where the model's own path says its packs are filed, and the ones the listing names there.
    root: Option<String>,
    packs: RefCell<Option<Result<Vec<Pack>, Rc<str>>>>,
    /// Cuts the pack list down while the picker is open.
    filter: RefCell<String>,
    /// The pack to play, as the user has it.
    wanted: RefCell<String>,
    pack: RefCell<Option<Fetch<Motions>>>,
    /// Which motion is playing, indexing [`Motions::named`]. None stands the model at rest, which
    /// is what a file being inspected shows; a character stands in its idle instead.
    motion: Cell<Option<usize>>,
    /// How far into it, in seconds.
    time: Cell<f32>,
    running: Cell<bool>,
}

impl Animation {
    pub fn new(model: &str) -> Self {
        let code = code(model);
        Self {
            skeleton: code.as_deref().and_then(skeleton_path),
            skin: RefCell::new(None),
            root: code.as_deref().and_then(pack_root),
            packs: RefCell::new(None),
            filter: RefCell::new(String::new()),
            wanted: RefCell::new(code.as_deref().and_then(pack_path).unwrap_or_default()),
            pack: RefCell::new(None),
            motion: Cell::new(Some(0)),
            time: Cell::new(0.0),
            running: Cell::new(false),
        }
    }

    /// Asks for the skeleton, the listing and the pack, and takes up whichever has landed. Only
    /// called for a model that carries bone indices, so nothing is fetched for one that could not
    /// be posed.
    pub fn poll(&self, ctx: &egui::Context, backend: &Backend) {
        if let Some(path) = &self.skeleton {
            Fetch::poll(&mut self.skin.borrow_mut(), backend, path, |bytes| {
                let file = SkeletonBinary::read(Cursor::new(bytes.to_vec()))?;
                let skeleton = file.parse_skeleton()?;
                Ok(Skin::new(Rig::new(
                    skeleton.bones(),
                    skeleton.parent_indices(),
                    skeleton.reference_pose(),
                )))
            });
        }
        let mut held = self.packs.borrow_mut();
        if let Some(root) = &self.root
            && held.is_none()
        {
            *held = match backend.listing(&api_base(ctx)) {
                Listed::Loading => None,
                Listed::Ready(listing) => {
                    let packs = found(root, listing.under(root));
                    // The conventional pack is right for nearly every model but not for all of
                    // them, and a weapon is named none at all; either way the listing knows better.
                    let mut wanted = self.wanted.borrow_mut();
                    if !packs.iter().any(|pack| pack.path == *wanted)
                        && let Some(first) = packs.first()
                    {
                        first.path.clone_into(&mut wanted);
                    }
                    Some(Ok(packs))
                }
                Listed::Failed(why) => Some(Err(why)),
            };
        }
        drop(held);
        let wanted = self.wanted.borrow();
        if !wanted.is_empty() {
            Fetch::poll(&mut self.pack.borrow_mut(), backend, &wanted, Motions::read);
        }
    }

    /// Stands it where its own file put it, which is what a file being inspected should show.
    pub fn rest(&self) {
        self.motion.set(None);
    }

    /// Plays `path` from its first motion, since a pack picked by hand was picked to be watched.
    pub fn play(&self, path: &str) {
        path.clone_into(&mut self.wanted.borrow_mut());
        *self.pack.borrow_mut() = None;
        self.motion.set(Some(0));
        self.time.set(0.0);
    }

    /// Where the model stands this frame: one walk of the rig, and everything read off it.
    pub fn pose(&self, tables: &[Vec<String>], skeleton: bool) -> Pose {
        let skin = self.skin.borrow();
        let Some(skin) = skin.as_ref().and_then(Fetch::ready) else {
            return Pose {
                joints: tables
                    .iter()
                    .map(|table| vec![Mat4::IDENTITY; table.len()])
                    .collect(),
                ..Default::default()
            };
        };
        let pack = self.pack.borrow();
        let binding = self
            .motion
            .get()
            .and_then(|at| pack.as_ref().and_then(Fetch::ready)?.binding(at));
        let posed = match binding {
            Some(binding) => skin.rig.posed(binding, self.time.get()),
            None => skin.rig.world(skin.rig.reference()),
        };
        let (center, spread) = middle(&posed, skin.anchor);
        Pose {
            joints: tables
                .iter()
                .map(|table| skin.palette(table, &posed))
                .collect(),
            skeleton: match skeleton {
                true => skin.rig.batches(&posed, None),
                false => Vec::new(),
            },
            drift: center - skin.home,
            stretch: (spread - skin.spread).max(0.0),
        }
    }

    /// Which pack is loaded, which motion is playing, play and pause, and the scrubber that is also
    /// what advances the clock. Only the pickers are offered until a motion is picked: with none
    /// the model stands where its own file put it, and there is nothing to play.
    pub fn ui(&self, ui: &mut egui::Ui) {
        self.packs_ui(ui);
        let pack = self.pack.borrow();
        let Some(motions) = pack.as_ref().and_then(Fetch::ready) else {
            return;
        };
        let motion = self.motion.get();
        egui::ComboBox::from_id_salt("mdl_motion")
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
        let Some(binding) = self.motion.get().and_then(|at| motions.binding(at)) else {
            return;
        };

        let duration = binding.motion().duration().max(f32::EPSILON);
        let mut time = self.time.get().clamp(0.0, duration);
        if self.running.get() {
            time += ui.input(|input| input.stable_dt).min(duration);
            if time > duration {
                time -= duration;
            }
            // Nothing else asks for a frame while the pointer is still, so playback has to.
            ui.ctx().request_repaint();
        }
        let running = self.running.get();
        if ui.button(if running { "Pause" } else { "Play" }).clicked() {
            self.running.set(!running);
        }
        ui.add(
            egui::Slider::new(&mut time, 0.0..=duration)
                .fixed_decimals(2)
                .suffix(" s"),
        );
        self.time.set(time);
    }

    /// Every pack filed under the model's own animation directory. A human carries thousands, so
    /// the list is filtered rather than scrolled.
    fn packs_ui(&self, ui: &mut egui::Ui) {
        let packs = self.packs.borrow();
        let Some(Ok(packs)) = packs.as_ref() else {
            return;
        };
        let wanted = self.wanted.borrow().clone();
        let mut picked = None;
        egui::ComboBox::from_id_salt("mdl_pack")
            .selected_text(match packs.iter().find(|pack| pack.path == wanted) {
                Some(pack) => pack.label.as_str(),
                None => file_name(&wanted),
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
                                .selectable_label(pack.path == wanted, &pack.label)
                                .clicked()
                            {
                                picked = Some(pack.path.clone());
                            }
                        }
                    });
            });
        if let Some(path) = picked {
            self.play(&path);
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
                if let Some(Fetch::Failed(why)) = self.skin.borrow().as_ref() {
                    ui.label(RichText::new(why).color(Color32::LIGHT_RED));
                }
            }
            None => {
                ui.label(RichText::new("this model's path names no skeleton").weak());
            }
        }
        let mut wanted = self.wanted.borrow().clone();
        if ui
            .add(egui::TextEdit::singleline(&mut wanted).hint_text("animation pack"))
            .changed()
        {
            *self.wanted.borrow_mut() = wanted;
            *self.pack.borrow_mut() = None;
            self.motion.set(None);
            self.time.set(0.0);
        }
        if let Some(Fetch::Failed(why)) = self.pack.borrow().as_ref() {
            ui.label(RichText::new(why).color(Color32::LIGHT_RED));
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

/// The `m0911` of a model's path, which is what its skeleton and its animations are filed under.
pub fn code(model: &str) -> Option<String> {
    let name = file_name(model);
    let code = name.get(..5)?;
    let (letter, digits) = code.split_at(1);
    let known = matches!(letter, "c" | "m" | "d" | "w");
    (known && digits.bytes().all(|byte| byte.is_ascii_digit())).then(|| code.to_owned())
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
    use super::{Skin, code, found, pack_path, pack_root, skeleton_path};

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
