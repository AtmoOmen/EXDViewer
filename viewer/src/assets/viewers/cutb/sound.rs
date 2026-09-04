//! What a cutscene sounds: the entry each `C063` names out of a `.scd` container, and the voice
//! line the client builds a path for out of each `C048` subtitle key.
//!
//! Nothing plays until [`Stage::enable`] runs from inside a real click, as the placed sounds of a
//! zone do: a browser only grants an `AudioContext` a user gesture.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use ironworks::excel::Language;
use ironworks::file::scd::SoundContainer;

use crate::audio::{self, Mixer};
use crate::backend::Backend;
use crate::data::{FileProvider, FileProviderExt};
use crate::utils::TrackedPromise;

/// The language codes the client writes into a voice path, indexed by its own cutscene language.
/// `off_1427CCF50`, in the order `caption_slot` already reads a subtitle's lengths in.
fn voice_code(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Japanese => "ja",
        Language::English => "en",
        Language::German => "de",
        Language::French => "fr",
        Language::ChineseSimplified => "chs",
        Language::Korean => "ko",
        _ => return None,
    })
}

/// The voice files a subtitle key names, in the order to try them.
///
/// `sub_14185AE20` splits the key on `_` and spends its second, third and fourth parts on the
/// path, under the expansion the cutscene itself sits in. The `%c` before the language is the
/// speaker's sex, which nothing in the cutscene states, so both are offered.
pub fn voice_paths(key: &str, slug: &str, language: Language) -> Vec<String> {
    let Some(code) = voice_code(language) else {
        return Vec::new();
    };
    let key = key.to_ascii_lowercase();
    let mut parts = key.splitn(5, '_');
    let (Some(_), Some(quest), Some(line), Some(speaker)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Vec::new();
    };
    if quest.len() < 6 || line.is_empty() || speaker.is_empty() {
        return Vec::new();
    }
    let folder = &quest[..6];
    ['m', 'f']
        .into_iter()
        .map(|sex| {
            format!(
                "cut/{slug}/sound/{folder}/{quest}_{line}/\
                 vo_{quest}_{line}_{speaker}_{sex}_{code}.scd"
            )
        })
        .collect()
}

/// One sound a cutscene plays: when, out of which container, and which of its entries.
#[derive(Clone)]
pub struct Cue {
    pub at: f32,
    /// The containers to try, in order. A `C063` names one; a voice line names one per sex.
    pub paths: Vec<String>,
    pub entry: usize,
    /// What to call it in the log.
    pub label: String,
    /// How long to hold the voice, in frames. `None` lets it go once its own audio has played out.
    pub holds: Option<f32>,
}

enum Held {
    Reading(TrackedPromise<anyhow::Result<(String, SoundContainer)>>),
    Ready(String, SoundContainer),
    Failed,
}

pub struct Stage {
    /// One entry per cue, keyed by the first path it offers.
    held: HashMap<String, Held>,
    /// The entries already decoded, so a cue that fires twice decodes once.
    decoded: HashMap<(String, usize), Arc<audio::Decoded>>,
    mixer: Option<Mixer<u64>>,
    /// Each voice with the frame it opened on and how long its own audio runs, in frames, so a
    /// one-shot is let go once it has played out.
    voices: Vec<(u64, f32, f32)>,
    /// The voice carrying the track playing under the whole cutscene, where one is.
    under: Option<u64>,
    next: u64,
    enabled: bool,
    volume: f32,
    error: Option<String>,
    /// How many cues named a file that is not there.
    missing: usize,
}

impl Default for Stage {
    fn default() -> Self {
        Self {
            held: HashMap::new(),
            decoded: HashMap::new(),
            mixer: None,
            voices: Vec::new(),
            under: None,
            next: 0,
            enabled: false,
            volume: 0.7,
            error: None,
            missing: 0,
        }
    }
}

impl Stage {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn missing(&self) -> usize {
        self.missing
    }

    pub fn playing(&self) -> usize {
        self.mixer.as_ref().map_or(0, Mixer::playing)
    }

    /// How many of the cues offered so far have a container to play out of.
    pub fn read(&self) -> usize {
        self.held
            .values()
            .filter(|held| matches!(held, Held::Ready(..)))
            .count()
    }

    /// Creates the mixer and resumes it, both from inside the same click.
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
        self.silence();
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(mixer) = &mut self.mixer {
            mixer.set_master_volume(volume);
        }
    }

    /// Stops everything now: what the transport does on a pause or a seek, since a cutscene's own
    /// clock is scrubbable and the mixer's is not.
    pub fn silence(&mut self) {
        self.voices.clear();
        self.under = None;
        if let Some(mixer) = &mut self.mixer {
            mixer.stop_all();
        }
    }

    /// Reads whatever a cue needs before it is due, so the sound lands on the frame it is cued on
    /// rather than whenever the fetch comes back.
    pub fn want(&mut self, backend: &Backend, cues: &[Cue]) {
        for cue in cues {
            let Some(key) = cue.paths.first() else {
                continue;
            };
            if self.held.contains_key(key) {
                continue;
            }
            let files = backend.files().clone();
            let paths = cue.paths.clone();
            self.held.insert(
                key.clone(),
                Held::Reading(TrackedPromise::spawn_local(async move {
                    read(files, paths).await
                })),
            );
        }
    }

    /// Takes in whatever finished reading and lets go of the voices that have played out.
    pub fn poll(&mut self, time: f32) {
        for held in self.held.values_mut() {
            if !matches!(held, Held::Reading(_)) {
                continue;
            }
            let Held::Reading(promise) = std::mem::replace(held, Held::Failed) else {
                unreachable!()
            };
            *held = match promise.try_take() {
                Ok(Ok((path, container))) => Held::Ready(path, container),
                Ok(Err(why)) => {
                    log::warn!("cutb: sound read failed: {why}");
                    self.missing += 1;
                    Held::Failed
                }
                Err(promise) => Held::Reading(promise),
            };
        }

        self.voices
            .retain(|(_, at, runs)| time >= *at && time - at < *runs);
        let live: Vec<u64> = self.voices.iter().map(|(id, ..)| *id).collect();
        self.under = self.under.filter(|id| live.contains(id));
        if let Some(mixer) = &mut self.mixer {
            mixer.retain(|id| live.contains(id));
        }
    }

    /// Plays a cue, if its container has been read. A cue whose file is still coming is dropped
    /// rather than played late.
    pub fn fire(&mut self, cue: &Cue, time: f32) {
        self.start(cue, time);
    }

    /// Keeps one track playing under the whole cutscene, which is where a quest's own music sits.
    pub fn under(&mut self, cue: &Cue, time: f32) {
        if self.under.is_none() {
            self.under = self.start(cue, time);
        }
    }

    /// Whether a track is playing under the cutscene now.
    pub fn sounding_under(&self) -> bool {
        self.under.is_some()
    }

    fn start(&mut self, cue: &Cue, time: f32) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let key = cue.paths.first()?;
        let Some(Held::Ready(path, container)) = self.held.get(key) else {
            return None;
        };
        let slot = (path.clone(), cue.entry);
        if !self.decoded.contains_key(&slot) {
            let Some(entry) = container.entries().get(cue.entry) else {
                log::warn!(
                    "cutb: {path} holds {} entries, not {}",
                    container.entries().len(),
                    cue.entry + 1
                );
                self.missing += 1;
                return None;
            };
            match audio::decode_data(entry.format(), entry.data()) {
                Ok(decoded) => {
                    self.decoded.insert(slot.clone(), Arc::new(decoded));
                }
                Err(why) => {
                    log::warn!("cutb: {path} entry {} did not decode: {why}", cue.entry);
                    self.missing += 1;
                    return None;
                }
            }
        }
        let audio = self.decoded[&slot].clone();
        let mixer = self.mixer.as_mut()?;
        let id = self.next;
        self.next += 1;
        if let Err(why) = mixer.play(id, audio.clone(), 1.0) {
            log::warn!("cutb: {path} did not play: {why}");
            return None;
        }
        self.voices
            .push((id, time, cue.holds.unwrap_or_else(|| runs_for(&audio))));
        log::info!("cutb: {} plays {path}#{} at frame {time:.0}", cue.label, cue.entry);
        Some(id)
    }
}

/// How long a decoded track runs, in the cutscene's own frames. A looping track is let go after
/// one pass, since nothing in a cue says how many it takes.
fn runs_for(audio: &audio::Decoded) -> f32 {
    let frames = audio.samples.len() / usize::from(audio.channels).max(1);
    frames as f32 / audio.sample_rate.max(1) as f32 * super::play::FRAMES_A_SECOND
}

async fn read(
    files: Rc<dyn FileProvider>,
    paths: Vec<String>,
) -> anyhow::Result<(String, SoundContainer)> {
    let mut last = None;
    for path in paths {
        match files.file::<SoundContainer>(&path).await {
            Ok(container) => return Ok((path, container)),
            Err(why) => last = Some(anyhow::anyhow!("{path}: {why}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("a cue naming no file")))
}

impl Drop for Stage {
    fn drop(&mut self) {
        self.silence();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_key_names_one_voice_file_per_sex_under_its_own_expansion() {
        assert_eq!(
            voice_paths("TEXT_HEAVNC110_02382_MOGZIN_000_110", "ex4", Language::English),
            [
                "cut/ex4/sound/heavnc/heavnc110_02382/vo_heavnc110_02382_mogzin_m_en.scd",
                "cut/ex4/sound/heavnc/heavnc110_02382/vo_heavnc110_02382_mogzin_f_en.scd",
            ]
        );
    }

    #[test]
    fn a_key_with_no_speaker_names_nothing() {
        assert!(voice_paths("TEXT_HEAVNC110_02382", "ex4", Language::English).is_empty());
    }

    #[test]
    fn a_language_the_client_rejects_names_nothing() {
        assert!(
            voice_paths(
                "TEXT_HEAVNC110_02382_MOGZIN_000_110",
                "ex4",
                Language::ChineseTraditional
            )
            .is_empty()
        );
    }
}
