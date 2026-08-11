//! Posing a model on the skeleton it is skinned to.
//!
//! A mesh's blend indices name slots of its own bone table, and that table names bones the way a
//! skeleton does, so the palette a skinned shader reads is matched up by name rather than by
//! position. Each joint carries the pose a motion puts its bone in against the pose the model is
//! stored in, which leaves a bone the skeleton does not name standing where the file put it.
//!
//! The skeleton and the pack of motions are both guessed from the model's own path and fetched on
//! the first frame that draws a skinned mesh, the way the model's `.imc` is.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Cursor;

use anyhow::Result;
use egui::{Color32, RichText};
use glam::Mat4;
use ironworks::file::File;
use ironworks::file::pap::{AnimationPack, Binding};
use ironworks::file::sklb::SkeletonBinary;

use super::super::skeleton::{Placement, Rig};
use super::super::{link, section};
use crate::backend::Backend;
use crate::utils::{TrackedPromise, file_name};

/// The rig a model is skinned to, ready to answer a mesh's bone table with a palette.
pub struct Skin {
    rig: Rig,
    /// Where each bone rests, inverted: what takes a vertex out of the pose the model is stored in.
    rest: Vec<Mat4>,
    /// Which bone the skeleton calls each name.
    named: HashMap<String, usize>,
}

impl Skin {
    fn new(rig: Rig) -> Self {
        let rest = rig
            .world(rig.reference())
            .iter()
            .map(|placement| placement.matrix().inverse())
            .collect();
        let named = rig
            .names()
            .iter()
            .enumerate()
            .map(|(bone, name)| (name.clone(), bone))
            .collect();
        Self { rig, rest, named }
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

/// What plays a model: the skeleton it is skinned to, a pack of motions, and the clock.
pub struct Animation {
    /// Where the model's own path says its skeleton is, and the rig that came of it.
    skeleton: Option<String>,
    skin: RefCell<Option<Fetch<Skin>>>,
    /// The pack to play, as the user has it.
    wanted: RefCell<String>,
    pack: RefCell<Option<Fetch<Motions>>>,
    /// Which motion is playing, indexing [`Motions::named`].
    motion: Cell<usize>,
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
            wanted: RefCell::new(code.as_deref().and_then(pack_path).unwrap_or_default()),
            pack: RefCell::new(None),
            motion: Cell::new(0),
            time: Cell::new(0.0),
            running: Cell::new(false),
        }
    }

    /// Asks for the skeleton and the pack, and takes up whichever has landed. Only called for a
    /// model that carries bone indices, so nothing is fetched for one that could not be posed.
    pub fn poll(&self, backend: &Backend) {
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
        let wanted = self.wanted.borrow();
        if !wanted.is_empty() {
            Fetch::poll(&mut self.pack.borrow_mut(), backend, &wanted, Motions::read);
        }
    }

    /// The palette each mesh's blend indices read, in the model's own space.
    pub fn palettes(&self, tables: &[Vec<String>]) -> Vec<Vec<Mat4>> {
        let skin = self.skin.borrow();
        let Some(skin) = skin.as_ref().and_then(Fetch::ready) else {
            return tables
                .iter()
                .map(|table| vec![Mat4::IDENTITY; table.len()])
                .collect();
        };
        let pack = self.pack.borrow();
        let binding = pack
            .as_ref()
            .and_then(Fetch::ready)
            .and_then(|motions| motions.binding(self.motion.get()));
        let posed = match binding {
            Some(binding) => skin.rig.posed(binding, self.time.get()),
            None => skin.rig.world(skin.rig.reference()),
        };
        tables
            .iter()
            .map(|table| skin.palette(table, &posed))
            .collect()
    }

    /// Which motion is playing, play and pause, and the scrubber that is also what advances the
    /// clock.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let pack = self.pack.borrow();
        let Some(motions) = pack.as_ref().and_then(Fetch::ready) else {
            return;
        };
        let motion = self.motion.get();
        let Some(binding) = motions.binding(motion) else {
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

        egui::ComboBox::from_id_salt("mdl_motion")
            .selected_text(&motions.named[motion].0)
            .show_ui(ui, |ui| {
                for (at, (name, _)) in motions.named.iter().enumerate() {
                    if ui.selectable_label(self.motion.get() == at, name).clicked() {
                        self.motion.set(at);
                        time = 0.0;
                    }
                }
            });
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
            self.motion.set(0);
            self.time.set(0.0);
        }
        if let Some(Fetch::Failed(why)) = self.pack.borrow().as_ref() {
            ui.label(RichText::new(why).color(Color32::LIGHT_RED));
        }
    }
}

/// The `m0911` of a model's path, which is what its skeleton and its animations are filed under.
fn code(model: &str) -> Option<String> {
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

/// The pack a model class idles from. A weapon has none of its own: it is moved by whoever holds
/// it.
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

#[cfg(test)]
mod tests {
    use glam::{Mat4, Vec3};
    use ironworks::file::sklb::Transform;

    use super::super::super::skeleton::Rig;
    use super::{Skin, code, pack_path, skeleton_path};

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
    }
}
