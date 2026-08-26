use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use rodio::{ChannelCount, DeviceSinkBuilder, MixerDeviceSink, SampleRate, Source};
use rustfft::{Fft, FftPlanner, num_complex::Complex};

use super::Decoded;

/// Native audio output via rodio
pub struct Player {
    device: MixerDeviceSink,
    sink: Option<rodio::Player>,
    audio: Option<Arc<Decoded>>,
    position: Arc<AtomicU64>,
    volume: f32,
    spectrum: RefCell<Spectrum>,
}

impl Player {
    pub fn new() -> Result<Self> {
        Ok(Self {
            device: DeviceSinkBuilder::open_default_sink()?,
            sink: None,
            audio: None,
            position: Arc::new(AtomicU64::new(0)),
            volume: 1.0,
            spectrum: RefCell::new(Spectrum::new()),
        })
    }

    /// `_announce` mirrors the web backend's OS media-session flag; native has no such surface
    /// yet (souvlaki is deferred), so it is ignored.
    pub fn play(&mut self, audio: Decoded, _announce: bool) -> Result<()> {
        self.audio = Some(Arc::new(audio));
        self.start_from(0)
    }

    pub fn seek(&mut self, seconds: f64) {
        if let Some(audio) = &self.audio {
            let frame = (seconds.max(0.0) * f64::from(audio.sample_rate)) as u64;
            let _ = self.start_from(frame);
        }
    }

    fn start_from(&mut self, frame: u64) -> Result<()> {
        let Some(audio) = self.audio.clone() else {
            return Ok(());
        };
        self.position.store(frame, Ordering::Relaxed);
        let sink = rodio::Player::connect_new(self.device.mixer());
        sink.set_volume(self.volume);
        sink.append(LoopingSource::new(audio, frame, self.position.clone()));
        self.sink = Some(sink);
        Ok(())
    }

    pub fn position(&self) -> f64 {
        match &self.audio {
            Some(audio) => {
                self.position.load(Ordering::Relaxed) as f64 / f64::from(audio.sample_rate)
            }
            None => 0.0,
        }
    }

    pub fn duration(&self) -> f64 {
        match &self.audio {
            Some(audio) => {
                (audio.samples.len() / audio.channels as usize) as f64
                    / f64::from(audio.sample_rate)
            }
            None => 0.0,
        }
    }

    pub fn pause(&self) {
        if let Some(sink) = &self.sink {
            sink.pause();
        }
    }

    pub fn resume(&self) {
        if let Some(sink) = &self.sink {
            sink.play();
        }
    }

    pub fn stop(&mut self) {
        self.sink = None;
        self.audio = None;
        self.position.store(0, Ordering::Relaxed);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(sink) = &self.sink {
            sink.set_volume(volume);
        }
    }

    /// No-op on native; OS media controls are a later addition.
    pub fn set_metadata(&self, _title: &str) {}

    /// No-op on native; on web this resumes the audio context in a user gesture.
    pub fn unlock(&self) {}

    pub fn spectrum(&self, out: &mut [u8]) {
        let Some(audio) = &self.audio else {
            out.fill(0);
            return;
        };
        let at = self.position.load(Ordering::Relaxed) as usize;
        self.spectrum.borrow_mut().read(audio, at, out);
    }

    /// No OS media controls on the native backend (souvlaki is deferred).
    pub fn take_media_action(&mut self) {}

    pub fn is_playing(&self) -> bool {
        self.sink
            .as_ref()
            .is_some_and(|sink| !sink.empty() && !sink.is_paused())
    }
}

/// Several looping voices sharing one output device, each with its own gain, for a scene playing
/// more than one ambient sound at once.
pub struct Mixer<K> {
    device: MixerDeviceSink,
    voices: HashMap<K, (rodio::Player, f32)>,
    master: f32,
}

impl<K: Eq + Hash> Mixer<K> {
    pub fn new() -> Result<Self> {
        Ok(Self {
            device: DeviceSinkBuilder::open_default_sink()?,
            voices: HashMap::new(),
            master: 1.0,
        })
    }

    /// No-op on native; the web backend needs this called from inside a user gesture instead.
    pub fn unlock(&self) {}

    pub fn set_master_volume(&mut self, volume: f32) {
        self.master = volume;
        for (sink, gain) in self.voices.values() {
            sink.set_volume(gain * self.master);
        }
    }

    pub fn is_playing(&self, key: &K) -> bool {
        self.voices.contains_key(key)
    }

    pub fn playing(&self) -> usize {
        self.voices.len()
    }

    /// Starts `key` looping, unless it already is.
    pub fn play(&mut self, key: K, audio: Arc<Decoded>, gain: f32) -> Result<()> {
        if self.voices.contains_key(&key) {
            return Ok(());
        }
        let sink = rodio::Player::connect_new(self.device.mixer());
        sink.set_volume(gain * self.master);
        sink.append(LoopingSource::new(audio, 0, Arc::new(AtomicU64::new(0))));
        self.voices.insert(key, (sink, gain));
        Ok(())
    }

    pub fn set_gain(&mut self, key: &K, gain: f32) {
        if let Some((sink, held)) = self.voices.get_mut(key) {
            *held = gain;
            sink.set_volume(gain * self.master);
        }
    }

    /// Stops whatever `keep` does not hold true for.
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.voices.retain(|key, _| keep(key));
    }

    pub fn stop_all(&mut self) {
        self.voices.clear();
    }
}

/// Frames the transform runs over. Half of them are bins, which is the length the visualizer reads
/// and maps across the whole band, so a shorter window would leave its top bars empty.
const WINDOW: usize = 8192;

/// The web backend takes its bars off an `AnalyserNode`; this stands in for one, reading the
/// decoded track around the play position rather than the output the device is mixing.
struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex<f32>>,
}

impl Spectrum {
    fn new() -> Self {
        Self {
            fft: FftPlanner::new().plan_fft_forward(WINDOW),
            buffer: vec![Complex::ZERO; WINDOW],
        }
    }

    fn read(&mut self, audio: &Decoded, at: usize, out: &mut [u8]) {
        let channels = usize::from(audio.channels);
        if channels == 0 {
            out.fill(0);
            return;
        }
        let frames = audio.samples.len() / channels;
        let looping = audio
            .loop_start
            .zip(audio.loop_end)
            .map(|(start, end)| (start as usize, end as usize))
            .filter(|(start, end)| start < end);
        let start = at.saturating_sub(WINDOW / 2);
        for (offset, value) in self.buffer.iter_mut().enumerate() {
            let mut frame = start + offset;
            // A window straddling the loop point reads what will actually be heard next rather
            // than the tail past it, which on most tracks is silence.
            if let Some((from, to)) = looping
                && frame >= to
            {
                frame = from + (frame - from) % (to - from);
            }
            let sample = match frame < frames {
                true => audio.samples[frame * channels..(frame + 1) * channels]
                    .iter()
                    .sum::<f32>()
                    / channels as f32,
                false => 0.0,
            };
            let hann =
                0.5 - 0.5 * (std::f32::consts::TAU * offset as f32 / (WINDOW - 1) as f32).cos();
            *value = Complex::new(sample * hann, 0.0);
        }
        self.fft.process(&mut self.buffer);

        let bins = out.len().min(WINDOW / 2);
        let scale = 2.0 / WINDOW as f32;
        for (slot, value) in out[..bins].iter_mut().zip(&self.buffer[..bins]) {
            let decibels = (value.norm() * scale).max(f32::MIN_POSITIVE).log10() * 20.0;
            *slot = (((decibels + 100.0) / 70.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        out[bins..].fill(0);
    }
}

struct LoopingSource {
    audio: Arc<Decoded>,
    index: usize,
    loop_region: Option<(usize, usize)>,
    channels: usize,
    position: Arc<AtomicU64>,
}

impl LoopingSource {
    fn new(audio: Arc<Decoded>, start_frame: u64, position: Arc<AtomicU64>) -> Self {
        let channels = audio.channels as usize;
        let loop_region = audio
            .loop_start
            .zip(audio.loop_end)
            .map(|(start, end)| (start as usize * channels, end as usize * channels));
        Self {
            index: start_frame as usize * channels,
            loop_region,
            channels,
            position,
            audio,
        }
    }
}

impl Iterator for LoopingSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if let Some((start, end)) = self.loop_region
            && self.index >= end
        {
            self.index = start;
        }
        let sample = self.audio.samples.get(self.index).copied();
        if sample.is_some() {
            self.index += 1;
            self.position
                .store((self.index / self.channels) as u64, Ordering::Relaxed);
        }
        sample
    }
}

impl Source for LoopingSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(self.audio.channels).unwrap()
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(self.audio.sample_rate).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        if self.loop_region.is_some() {
            return None;
        }
        let frames = self.audio.samples.len() as f64 / f64::from(self.audio.channels);
        Some(Duration::from_secs_f64(
            frames / f64::from(self.audio.sample_rate),
        ))
    }
}

#[cfg(test)]
mod test {
    use super::{Spectrum, WINDOW};
    use crate::audio::Decoded;

    /// A pure tone has to land in the bin its frequency names, since the visualizer maps the
    /// output across the whole band on exactly that assumption.
    #[test]
    fn tone_lands_in_its_own_bin() {
        let rate = 44_100;
        let tone = 1_000.0;
        let samples = (0..WINDOW * 2)
            .map(|at| {
                (std::f32::consts::TAU * tone * at as f32 / rate as f32).sin()
            })
            .collect();
        let audio = Decoded {
            samples,
            channels: 1,
            sample_rate: rate,
            loop_start: None,
            loop_end: None,
        };
        let mut out = [0u8; WINDOW / 2];
        Spectrum::new().read(&audio, WINDOW, &mut out);

        let loudest = out
            .iter()
            .enumerate()
            .max_by_key(|(_, level)| **level)
            .map(|(at, _)| at)
            .unwrap();
        let nyquist = rate as f32 / 2.0;
        let found = loudest as f32 / out.len() as f32 * nyquist;
        assert!((found - tone).abs() < nyquist / out.len() as f32 * 2.0, "peak at {found} Hz");
    }
}
