//! Positional playback of a zone's placed `.lgb` `Sound` instances: distance-based volume off the
//! range and volume the placement's own record states, mixed down through as many voices as are in
//! range at once.
//!
//! Nothing here plays until [`SoundStage::enable`] runs from inside a real click: a browser only
//! grants an `AudioContext` a user gesture, and native gains nothing by starting earlier.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use glam::Vec3;
use ironworks::file::layer::Sound;
use ironworks::file::scd::SoundContainer;

use crate::audio::{self, Mixer};
use crate::backend::Backend;
use crate::data::{FileProvider, FileProviderExt};
use crate::utils::TrackedPromise;

/// How an instance is reached from the top of the tree: the placement at the top, then an index per
/// shared group under it. Matches the key every other placed thing in the scene carries.
pub type Key = (u32, [u8; 4]);

/// Voices allowed to play at once. A zone's placed sounds run into the thousands; only the ones
/// nearest the camera are ever audible.
const MAX_VOICES: usize = 16;

struct Placement {
    position: Vec3,
    path: String,
    inner_radius: f32,
    outer_radius: f32,
    volume_a: f32,
    volume_b: f32,
    no_far_clip: bool,
    key: Key,
}

enum Decode {
    Fetching(TrackedPromise<anyhow::Result<audio::Decoded>>),
    Ready(Arc<audio::Decoded>),
    Failed,
}

pub struct SoundStage {
    placements: Vec<Placement>,
    decode: HashMap<String, Decode>,
    mixer: Option<Mixer<Key>>,
    enabled: bool,
    volume: f32,
    error: Option<String>,
}

impl Default for SoundStage {
    fn default() -> Self {
        Self {
            placements: Vec::new(),
            decode: HashMap::new(),
            mixer: None,
            enabled: false,
            volume: 0.6,
            error: None,
        }
    }
}

impl SoundStage {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn placed(&self) -> usize {
        self.placements.len()
    }

    pub fn playing(&self) -> usize {
        self.mixer.as_ref().map_or(0, Mixer::playing)
    }

    /// A `Sound` instance the walk placed, kept where its kind states a range and volume to play it
    /// over. Kinds with nothing of their own to play (the obstructions, and any this corpus never
    /// saw) are dropped here rather than carried with nothing to compute a gain from.
    pub fn collect(&mut self, sound: &Sound, position: Vec3, key: Key) {
        if sound.asset_path().is_empty() {
            return;
        }
        let Some(attenuation) = sound.attenuation() else {
            return;
        };
        self.placements.push(Placement {
            position,
            path: sound.asset_path().clone(),
            inner_radius: attenuation.inner_radius(),
            outer_radius: attenuation.outer_radius(),
            volume_a: attenuation.volume_a(),
            volume_b: attenuation.volume_b(),
            no_far_clip: sound.no_far_clip(),
            key,
        });
    }

    /// Creates the mixer and resumes it, both from inside the same click: the resume is what a
    /// browser accepts as the user gesture, and doing it anywhere else would leave the context
    /// suspended with nothing left to unsuspend it later.
    pub fn enable(&mut self) {
        self.error = None;
        if self.mixer.is_none() {
            match Mixer::new() {
                Ok(mut mixer) => {
                    mixer.set_master_volume(self.volume);
                    self.mixer = Some(mixer);
                }
                Err(why) => {
                    self.error = Some(why.to_string());
                    return;
                }
            }
        }
        if let Some(mixer) = &self.mixer {
            mixer.unlock();
        }
        self.enabled = true;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
        if let Some(mixer) = &mut self.mixer {
            mixer.stop_all();
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(mixer) = &mut self.mixer {
            mixer.set_master_volume(volume);
        }
    }

    /// Runs every frame: takes in whatever finished decoding, and where sound is enabled, keeps the
    /// nearest placements playing at the gain their own range and volume state.
    pub fn poll(&mut self, backend: &Backend, eye: Vec3) {
        for decode in self.decode.values_mut() {
            if !matches!(decode, Decode::Fetching(_)) {
                continue;
            }
            let Decode::Fetching(promise) = std::mem::replace(decode, Decode::Failed) else {
                unreachable!()
            };
            *decode = match promise.try_take() {
                Ok(Ok(decoded)) => Decode::Ready(Arc::new(decoded)),
                Ok(Err(why)) => {
                    log::warn!("assets/layer/scene: sound decode failed: {why}");
                    Decode::Failed
                }
                Err(promise) => Decode::Fetching(promise),
            };
        }

        if !self.enabled {
            return;
        }
        let Some(mixer) = &mut self.mixer else {
            return;
        };

        let mut ranked: Vec<(bool, f32, usize)> = self
            .placements
            .iter()
            .enumerate()
            .map(|(index, placement)| {
                (
                    !placement.no_far_clip,
                    placement.position.distance(eye),
                    index,
                )
            })
            .filter(|&(_, distance, index)| {
                let placement = &self.placements[index];
                placement.no_far_clip || distance <= placement.outer_radius
            })
            .collect();
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        ranked.truncate(MAX_VOICES);

        let keep: HashSet<Key> = ranked
            .iter()
            .map(|&(_, _, index)| self.placements[index].key)
            .collect();
        mixer.retain(|key| keep.contains(key));

        for (_, distance, index) in ranked {
            let placement = &self.placements[index];
            let gain = gain(
                distance,
                placement.inner_radius,
                placement.outer_radius,
                placement.volume_a,
                placement.volume_b,
            );
            match self.decode.get(&placement.path) {
                Some(Decode::Ready(audio)) => {
                    let audio = audio.clone();
                    match mixer.is_playing(&placement.key) {
                        true => mixer.set_gain(&placement.key, gain),
                        false => {
                            if let Err(why) = mixer.play(placement.key, audio, gain) {
                                log::warn!("assets/layer/scene: sound play failed: {why}");
                            }
                        }
                    }
                }
                Some(Decode::Fetching(_) | Decode::Failed) => {}
                None => {
                    let files = backend.files().clone();
                    let path = placement.path.clone();
                    self.decode.insert(
                        placement.path.clone(),
                        Decode::Fetching(TrackedPromise::spawn_local(async move {
                            fetch_decode(files, path).await
                        })),
                    );
                }
            }
        }
    }
}

/// Full volume inside the inner radius, none past the outer, and a straight ramp between: nothing
/// in the record states a curve shape, so this is the plainest one that fits the two points it does
/// state. The two `[0, 1]` multipliers a placement carries are applied together, since nothing
/// distinguishes which of them is "the" volume.
fn gain(distance: f32, inner: f32, outer: f32, volume_a: f32, volume_b: f32) -> f32 {
    let curve = match outer > inner {
        true => ((outer - distance) / (outer - inner)).clamp(0.0, 1.0),
        false => match distance <= outer {
            true => 1.0,
            false => 0.0,
        },
    };
    curve * volume_a * volume_b
}

async fn fetch_decode(files: Rc<dyn FileProvider>, path: String) -> anyhow::Result<audio::Decoded> {
    let container = files.file::<SoundContainer>(&path).await?;
    let entry = container
        .entries()
        .first()
        .ok_or_else(|| anyhow::anyhow!("{path}: no audio streams"))?;
    audio::decode_data(entry.format(), entry.data())
}

impl Drop for SoundStage {
    fn drop(&mut self) {
        if let Some(mixer) = &mut self.mixer {
            mixer.stop_all();
        }
    }
}

#[cfg(test)]
mod test {
    use super::gain;

    #[test]
    fn full_volume_inside_the_inner_radius() {
        assert_eq!(gain(0.0, 5.0, 70.0, 0.8, 0.5), 0.4);
        assert_eq!(gain(5.0, 5.0, 70.0, 0.8, 0.5), 0.4);
    }

    #[test]
    fn silent_past_the_outer_radius() {
        assert_eq!(gain(70.0, 5.0, 70.0, 0.8, 0.5), 0.0);
        assert_eq!(gain(1000.0, 5.0, 70.0, 0.8, 0.5), 0.0);
    }

    #[test]
    fn ramps_between_the_two() {
        assert_eq!(gain(50.0, 0.0, 100.0, 1.0, 1.0), 0.5);
        assert_eq!(gain(25.0, 0.0, 100.0, 1.0, 1.0), 0.75);
    }

    #[test]
    fn a_degenerate_range_steps_instead_of_ramping() {
        assert_eq!(gain(39.0, 40.0, 40.0, 1.0, 1.0), 1.0);
        assert_eq!(gain(41.0, 40.0, 40.0, 1.0, 1.0), 0.0);
    }
}
