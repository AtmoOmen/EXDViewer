use std::cell::RefCell;
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
    spectrum: RefCell<SpectrumAnalyzer>,
}

impl Player {
    pub fn new() -> Result<Self> {
        Ok(Self {
            device: DeviceSinkBuilder::open_default_sink()?,
            sink: None,
            audio: None,
            position: Arc::new(AtomicU64::new(0)),
            volume: 1.0,
            spectrum: RefCell::new(SpectrumAnalyzer::new()),
        })
    }

    pub fn play(&mut self, audio: Decoded) -> Result<()> {
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
        self.spectrum.borrow_mut().analyze(
            audio,
            self.position.load(Ordering::Relaxed) as usize,
            out,
        );
    }

    /// No OS media controls on the native backend (souvlaki is deferred).
    pub fn take_media_action(&mut self) {}

    pub fn is_playing(&self) -> bool {
        self.sink
            .as_ref()
            .is_some_and(|sink| !sink.empty() && !sink.is_paused())
    }
}

const SPECTRUM_SIZE: usize = 8192;

struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex<f32>>,
}

impl SpectrumAnalyzer {
    fn new() -> Self {
        let mut planner = FftPlanner::new();
        Self {
            fft: planner.plan_fft_forward(SPECTRUM_SIZE),
            buffer: vec![Complex::ZERO; SPECTRUM_SIZE],
        }
    }

    fn analyze(&mut self, audio: &Decoded, position: usize, out: &mut [u8]) {
        let channels = usize::from(audio.channels);
        if channels == 0 {
            out.fill(0);
            return;
        }
        let frame_count = audio.samples.len() / channels;
        let start = position.saturating_sub(SPECTRUM_SIZE / 2);
        let loop_region = audio
            .loop_start
            .zip(audio.loop_end)
            .map(|(start, end)| (start as usize, end as usize))
            .filter(|(start, end)| start < end);
        for (offset, value) in self.buffer.iter_mut().enumerate() {
            let mut frame = start + offset;
            if let Some((loop_start, loop_end)) = loop_region
                && frame >= loop_end
            {
                frame = loop_start + (frame - loop_start) % (loop_end - loop_start);
            }
            let sample = if frame < frame_count {
                let sample_start = frame * channels;
                audio.samples[sample_start..sample_start + channels]
                    .iter()
                    .sum::<f32>()
                    / channels as f32
            } else {
                0.0
            };
            let window = 0.5
                - 0.5 * (std::f32::consts::TAU * offset as f32 / (SPECTRUM_SIZE - 1) as f32).cos();
            *value = Complex::new(sample * window, 0.0);
        }

        self.fft.process(&mut self.buffer);
        let bin_count = out.len().min(SPECTRUM_SIZE / 2);
        let scale = 2.0 / SPECTRUM_SIZE as f32;
        for (output, value) in out[..bin_count].iter_mut().zip(&self.buffer[..bin_count]) {
            let decibels = (value.norm() * scale).max(f32::MIN_POSITIVE).log10() * 20.0;
            *output = (((decibels + 100.0) / 70.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        out[bin_count..].fill(0);
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
