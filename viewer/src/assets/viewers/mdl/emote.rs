//! Props, sound and vfx an emote's own timeline states, read out of the `.pap` its body motion
//! plays rather than out of anything the creator or the sheets name.
//!
//! A body motion is a Havok animation driven by a `.tmb` timeline embedded in the same `.pap`.
//! `C043`/`C198` summon a held prop, built the same way a weapon is
//! (`chara/weapon/w####/obj/body/b####/model/w####b####.mdl`); `C063` plays a sound; `C012`/`C173`
//! play a `.avfx`. None of the three name a bone of their own for a prop: measured against
//! `chara/xls/attachoffset/c0101.atch`, a tool held in the main hand (food, an axe, a hammer)
//! rests at `n_buki_r` with no offset, which is the same fallback a weapon takes with no `.atch`
//! tag resolved for it; a prop meant for the hip, the back or the off hand (a card, a drum, a
//! harp) is not, and nothing in the file says which is which, so this places every prop there.
//!
//! Timeline time is frames at a fixed 30 fps, measured across four packs by comparing `TMDH`'s own
//! duration against the Havok motion's, in seconds, that the same pack plays (330/11, 60/2,
//! 145/4.8333, 690/23 all divide out to exactly 30).

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use glam::{EulerRot, Mat4, Quat, Vec3};
use ironworks::file::File;
use ironworks::file::pap::AnimationPack;
use ironworks::file::tmb::{CommandKind, Item, Timeline};

use crate::audio::{self, Mixer};
use crate::backend::Backend;
use crate::utils::TrackedPromise;

const FPS: f32 = 30.0;

/// The bone a prop, or a vfx bound to the character or the prop it summons, hangs from when
/// nothing names an attach point for it.
const FALLBACK_BONE: &str = "n_buki_r";

/// The bone a vfx bound to the plain character (`BindType::Character`) hangs from with no bone id
/// of its own: the same bone a pose is centred on.
const ROOT_BONE: &str = "n_hara";

fn secs(frames: i32) -> f32 {
    frames as f32 / FPS
}

/// `chara/weapon/w####/obj/body/b####/model/w####b####.mdl`, the same path a real weapon is
/// carried as.
fn prop_model(set: i16, base: i16) -> String {
    let set = u16::try_from(set).unwrap_or(0);
    let base = u16::try_from(base).unwrap_or(0);
    format!("chara/weapon/w{set:04}/obj/body/b{base:04}/model/w{set:04}b{base:04}.mdl")
}

/// `C012`/`C173`'s `BindType`, out of VFXEditor's `C012.cs`: which of the two default (`-1`) bind
/// ids this corpus carries resolves to a bone. A non-default id is left unresolved, since nothing
/// in the file states which bone a numeric id names.
fn bind_bone(bind_type: u8, bind_id: i16) -> Option<&'static str> {
    if bind_id != -1 {
        return None;
    }
    match bind_type {
        0 => Some(ROOT_BONE), // Character
        // Weapon, Offhand, Summon (the prop this timeline itself summons).
        1..=3 => Some(FALLBACK_BONE),
        _ => None,
    }
}

fn vec3(values: &[f32], default: f32) -> Vec3 {
    match values {
        [x, y, z] => Vec3::new(*x, *y, *z),
        _ => Vec3::splat(default),
    }
}

fn vec4(values: &[f32], default: f32) -> [f32; 4] {
    match values {
        [x, y, z, w] => [*x, *y, *z, *w],
        _ => [default; 4],
    }
}

fn local_transform(scale: &[f32], rotation: &[f32], position: &[f32]) -> Mat4 {
    let rotation = vec3(rotation, 0.0);
    Mat4::from_scale_rotation_translation(
        vec3(scale, 1.0),
        Quat::from_euler(EulerRot::XYZ, rotation.x, rotation.y, rotation.z),
        vec3(position, 0.0),
    )
}

/// A held prop's window, model and material variant.
struct Prop {
    start: f32,
    end: f32,
    path: String,
    variant: u16,
}

/// A sound's own start, for firing once as the clock crosses it.
struct Sound {
    id: i16,
    start: f32,
    path: String,
}

/// A vfx's window, where it is bound, and its own tint.
pub struct Vfx {
    start: f32,
    end: f32,
    pub bone: &'static str,
    local: Mat4,
    tint: [f32; 4],
}

impl Vfx {
    pub fn local(&self, world: Mat4) -> Mat4 {
        world * self.local
    }

    pub fn tint(&self) -> [f32; 4] {
        self.tint
    }
}

/// Everything one motion's timeline states, read once and kept until the motion or the pack
/// changes.
#[derive(Default)]
struct Events {
    props: Vec<Prop>,
    sounds: Vec<Sound>,
    vfx: Vec<Vfx>,
}

impl Events {
    fn read(bytes: &[u8], animation_name: &str) -> anyhow::Result<Self> {
        let pack = AnimationPack::read(Cursor::new(bytes.to_vec()))?;
        let index = pack
            .animations()
            .iter()
            .position(|animation| animation.name() == animation_name)
            .ok_or_else(|| anyhow::anyhow!("{animation_name}: not in this pack"))?;
        let timeline = Timeline::read(Cursor::new(pack.timelines()[index].clone()))?;

        let mut events = Self::default();
        for item in timeline.items() {
            let Item::Command(command) = item else {
                continue;
            };
            let start = secs(i32::from(command.time()));
            match command.kind() {
                CommandKind::C043(c) => events.props.push(Prop {
                    start,
                    end: start + secs(c.duration()),
                    path: prop_model(c.weapon_id(), c.body_id()),
                    variant: u16::try_from(c.variant_id()).unwrap_or(0),
                }),
                CommandKind::C198(c) => events.props.push(Prop {
                    start,
                    end: start + secs(c.duration()),
                    path: prop_model(c.model_id(), c.body_id()),
                    variant: u16::try_from(c.variant()).unwrap_or(0),
                }),
                CommandKind::C063(c) => {
                    if let Some(path) = c.path() {
                        events.sounds.push(Sound {
                            id: command.id(),
                            start,
                            path: path.to_owned(),
                        });
                    }
                }
                CommandKind::C012(c) => {
                    if c.path().is_some()
                        && let Some(bone) = bind_bone(c.bind_type_1(), c.bind_id_1())
                    {
                        events.vfx.push(Vfx {
                            start,
                            end: start + secs(c.duration()),
                            bone,
                            local: local_transform(c.scale(), c.rotation(), c.position()),
                            tint: vec4(c.rgba(), 1.0),
                        });
                    }
                }
                CommandKind::C173(c) => {
                    if c.path().is_some()
                        && let Some(bone) = bind_bone(c.bind_type_1(), c.bind_id_1())
                    {
                        // No duration of its own: the command starts a loop and leaves it running
                        // rather than waiting on it, so the marker holds for a fixed span instead.
                        events.vfx.push(Vfx {
                            start,
                            end: start + 1.0,
                            bone,
                            local: Mat4::IDENTITY,
                            tint: [1.0; 4],
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(events)
    }

    fn active_prop(&self, time: f32) -> Option<&Prop> {
        self.props
            .iter()
            .find(|prop| time >= prop.start && time < prop.end.max(prop.start + f32::EPSILON))
    }

    fn active_vfx(&self, time: f32) -> impl Iterator<Item = &Vfx> {
        self.vfx
            .iter()
            .filter(move |vfx| time >= vfx.start && time < vfx.end)
    }

    /// Sounds whose start crossed between `since` (exclusive) and `time` (inclusive), wrapping
    /// once at the timeline's own duration for a motion that loops.
    fn due_sounds(&self, since: f32, time: f32) -> impl Iterator<Item = &Sound> {
        let wrapped = time < since;
        self.sounds.iter().filter(move |sound| match wrapped {
            false => sound.start > since && sound.start <= time,
            true => sound.start > since || sound.start <= time,
        })
    }
}

enum Fetch {
    Fetching(TrackedPromise<anyhow::Result<Vec<u8>>>),
    Ready(Events),
    Failed,
}

enum SoundFetch {
    Fetching(TrackedPromise<anyhow::Result<audio::Decoded>>),
    Ready(Arc<audio::Decoded>),
    Failed,
}

/// One emote's props, sound and vfx, tracked against the body motion currently playing: which
/// pack this was read from, the clock it was last polled at, and the voices its sounds are
/// playing through.
#[derive(Default)]
pub struct Cue {
    key: Option<(String, String)>,
    fetch: Option<Fetch>,
    last_time: f32,
    loop_count: u32,
    decode: HashMap<String, SoundFetch>,
    voices: Option<Mixer<(i16, u32)>>,
    voices_failed: bool,
}

impl Cue {
    /// Takes up whatever the body is playing, fetching and parsing its pack's timeline once, and
    /// fires whichever sounds the clock has crossed since the last poll.
    pub fn poll(&mut self, backend: &Backend, playing: Option<(String, String, f32)>) {
        let Some((pack, name, time)) = playing else {
            return;
        };
        let key = (pack.clone(), name.clone());
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.fetch = None;
            // Below any real command time, so a sound at frame 0 still counts as due once the
            // pack finishes fetching rather than needing a loop back around to be crossed.
            self.last_time = -1.0;
        }
        match &mut self.fetch {
            None => {
                let files = backend.files().clone();
                let wanted = pack.clone();
                self.fetch = Some(Fetch::Fetching(TrackedPromise::spawn_local(async move {
                    files.read(&wanted).await
                })));
            }
            Some(Fetch::Fetching(promise)) => {
                if let Some(result) = promise.try_get() {
                    self.fetch = Some(match result.as_ref().map_err(ToString::to_string) {
                        Ok(bytes) => match Events::read(bytes, &name) {
                            Ok(events) => Fetch::Ready(events),
                            Err(_) => Fetch::Failed,
                        },
                        Err(_) => Fetch::Failed,
                    });
                }
            }
            Some(_) => {}
        }

        let Some(Fetch::Ready(events)) = &self.fetch else {
            return;
        };
        if time < self.last_time {
            self.loop_count += 1;
        }
        let due: Vec<(i16, String)> = events
            .due_sounds(self.last_time, time)
            .map(|sound| (sound.id, sound.path.clone()))
            .collect();
        self.last_time = time;
        if due.is_empty() {
            return;
        }

        if self.voices.is_none() && !self.voices_failed {
            match Mixer::new() {
                Ok(mixer) => self.voices = Some(mixer),
                Err(why) => {
                    log::warn!("assets/mdl: no emote sound: {why}");
                    self.voices_failed = true;
                }
            }
        }
        let loop_count = self.loop_count;
        let Some(voices) = &mut self.voices else {
            return;
        };
        voices.unlock();
        voices.retain(|(_, held)| *held + 1 >= loop_count);
        for (id, path) in due {
            match self.decode.get(&path) {
                Some(SoundFetch::Ready(decoded)) => {
                    let decoded = decoded.clone();
                    if let Err(why) = voices.play((id, loop_count), decoded, 1.0) {
                        log::warn!("assets/mdl: emote sound play failed: {why}");
                    }
                }
                Some(SoundFetch::Fetching(_) | SoundFetch::Failed) => {}
                None => {
                    let files = backend.files().clone();
                    let wanted = path.clone();
                    self.decode.insert(
                        path,
                        SoundFetch::Fetching(TrackedPromise::spawn_local(async move {
                            let bytes = files.read(&wanted).await?;
                            let container =
                                ironworks::file::scd::SoundContainer::read(Cursor::new(bytes))?;
                            let entry = container
                                .entries()
                                .first()
                                .ok_or_else(|| anyhow::anyhow!("{wanted}: no audio streams"))?;
                            audio::decode_data(entry.format(), entry.data())
                        })),
                    );
                }
            }
        }
        for fetch in self.decode.values_mut() {
            if !matches!(fetch, SoundFetch::Fetching(_)) {
                continue;
            }
            let SoundFetch::Fetching(promise) = std::mem::replace(fetch, SoundFetch::Failed) else {
                unreachable!()
            };
            *fetch = match promise.try_take() {
                Ok(Ok(decoded)) => SoundFetch::Ready(Arc::new(decoded)),
                Ok(Err(why)) => {
                    log::warn!("assets/mdl: emote sound decode failed: {why}");
                    SoundFetch::Failed
                }
                Err(promise) => SoundFetch::Fetching(promise),
            };
        }
    }

    /// The model an emote's own timeline wants held right now, by the path it is worn as and its
    /// material variant.
    pub fn active_prop(&self, time: f32) -> Option<(String, u16)> {
        let Some(Fetch::Ready(events)) = &self.fetch else {
            return None;
        };
        events
            .active_prop(time)
            .map(|prop| (prop.path.clone(), prop.variant))
    }

    /// The vfx firing right now.
    pub fn active_vfx(&self, time: f32) -> impl Iterator<Item = &Vfx> {
        let ready = match &self.fetch {
            Some(Fetch::Ready(events)) => Some(events),
            _ => None,
        };
        ready
            .into_iter()
            .flat_map(move |events| events.active_vfx(time))
    }
}
