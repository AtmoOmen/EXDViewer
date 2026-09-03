//! `.scd` sound containers: the audio streams a bank holds, playable where the codec is one
//! ironworks can decode.

use std::cell::RefCell;
use std::io::Cursor;
use std::time::Duration;

use anyhow::Result;
use egui::{Color32, RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::scd::{Codec, SoundContainer};

use super::{Preview, facts, headers, section};
use crate::assets::Bytes;
use crate::audio::{self, Player};
use crate::utils::TrackedPromise;
use crate::utils::export;

enum PlayState {
    Idle,
    Decoding(usize, TrackedPromise<Result<audio::Decoded>>),
    Playing(usize),
}

thread_local! {
    // One audio backend for every sound container the Assets tab opens, not one per file: on the
    // web a `Player` owns an `AudioContext` and wasm-bindgen closures tied to it, and tearing one
    // down right as another spins up (switching files while a track plays) raced its `onended`
    // handler and threw "closure invoked recursively or after being dropped".
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
}

/// A sound container, decoded and ready to draw.
pub struct Rendered {
    name: String,
    identity: Vec<(&'static str, String)>,
    container: SoundContainer,
    state: RefCell<PlayState>,
    error: RefCell<Option<String>>,
    export: RefCell<Option<TrackedPromise<()>>>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let container = SoundContainer::read(Cursor::new(bytes.to_vec()))?;

    let identity = vec![
        ("Sounds", container.sound_count().to_string()),
        ("Tracks", container.track_count().to_string()),
        ("Streams", container.entries().len().to_string()),
    ];

    log::info!("assets/scd: {path} {} streams", container.entries().len());

    Ok(Preview::Scd(Box::new(Rendered {
        name: crate::utils::file_name(path)
            .trim_end_matches(".scd")
            .to_owned(),
        identity,
        container,
        state: RefCell::new(PlayState::Idle),
        error: RefCell::new(None),
        export: RefCell::new(None),
    })))
}

const COLUMNS: usize = 8;
const HEADERS: [&str; COLUMNS] = ["", "#", "Codec", "Ch", "Rate", "Bytes", "Loop", "Markers"];

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    file.poll();
    file.poll_export();
    let exporting = file.export.borrow().is_some();
    if !matches!(&*file.state.borrow(), PlayState::Idle) || exporting {
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }

    ui.horizontal(|ui| {
        section(ui, "音频流");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            let promise = export::menu(
                ui,
                "导出",
                None,
                exporting,
                vec![
                    export::Choice::named_bytes("原始音频流", || {
                        let entries = file.container.entries().to_vec();
                        audio::package(audio::export_native(&entries), &file.name)
                    })
                    .title("导出音频")
                    .hover("每个条目的原始编码字节"),
                    export::Choice::named_bytes("解码 WAV", || {
                        let entries = file.container.entries().to_vec();
                        audio::package(audio::export_wav(&entries)?, &file.name)
                    })
                    .title("导出音频")
                    .hover("保留全部声道，不混合"),
                ],
                egui::Vec2::ZERO,
            );
            if promise.is_some() {
                *file.export.borrow_mut() = promise;
            }
        });
    });
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        egui::Grid::new("scd_entries")
            .num_columns(COLUMNS)
            .striped(true)
            .show(ui, |ui| {
                headers(ui, &HEADERS);
                for index in 0..file.container.entries().len() {
                    file.row(ui, index);
                }
            });
    });

    if let Some(error) = file.error.borrow().as_deref() {
        ui.add_space(8.0);
        ui.colored_label(Color32::RED, error);
    }
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "scd_identity", &self.identity));
    }

    fn row(&self, ui: &mut egui::Ui, index: usize) {
        let entry = &self.container.entries()[index];
        let state = self.state.borrow();
        let playing = matches!(&*state, PlayState::Playing(playing) if *playing == index);
        let decoding = matches!(&*state, PlayState::Decoding(decoding, _) if *decoding == index);
        drop(state);

        let playable = matches!(entry.format(), Codec::OggVorbis | Codec::Hca | Codec::MsAdpcm);
        let glyph = match (playing, decoding) {
            (true, _) => "⏹",
            (_, true) => "…",
            _ => "▶",
        };
        if ui.add_enabled(playable, egui::Button::new(glyph)).clicked() {
            self.toggle(index);
        }
        ui.label(entry.slot().to_string());
        ui.label(codec_name(entry.format()));
        ui.label(entry.channel_count().to_string());
        ui.label(format!("{} Hz", entry.sample_rate()));
        ui.label(Bytes(entry.data().len()).to_string());
        if entry.loop_end() > 0 {
            ui.label(format!("{}..{}", entry.loop_start(), entry.loop_end()));
        } else {
            ui.label(RichText::new("-").weak());
        }
        if entry.markers().is_empty() {
            ui.label(RichText::new("-").weak());
        } else {
            ui.label(entry.markers().len().to_string());
        }
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        ui.end_row();
    }

    fn toggle(&self, index: usize) {
        let already = matches!(
            &*self.state.borrow(),
            PlayState::Playing(playing) | PlayState::Decoding(playing, _) if *playing == index
        );
        *self.error.borrow_mut() = None;
        PLAYER.with_borrow_mut(|player| {
            if let Some(player) = player.as_mut() {
                player.stop();
            }
        });
        if already {
            *self.state.borrow_mut() = PlayState::Idle;
            return;
        }
        // The audio backend has to be created (and, on the web, its context resumed) inside the
        // click itself; a browser only grants that from a real user gesture.
        let unlocked: Result<()> = PLAYER.with_borrow_mut(|player| {
            if player.is_none() {
                *player = Some(Player::new()?);
            }
            player.as_ref().unwrap().unlock();
            Ok(())
        });
        if let Err(error) = unlocked {
            *self.error.borrow_mut() = Some(error.to_string());
            return;
        }

        let Some(entry) = self.container.entries().get(index).cloned() else {
            return;
        };
        let promise = TrackedPromise::spawn_local(async move { audio::decode(&entry) });
        *self.state.borrow_mut() = PlayState::Decoding(index, promise);
    }

    /// Advances a pending decode to playback, and notices when playback has run its course.
    fn poll(&self) {
        // A match scrutinee keeps its temporaries alive for the whole match, so the borrow this
        // takes has to end before the arms below can borrow `state` again themselves.
        let previous = std::mem::replace(&mut *self.state.borrow_mut(), PlayState::Idle);
        let taken = match previous {
            PlayState::Decoding(index, promise) => match promise.try_take() {
                Ok(result) => Some((index, result)),
                Err(promise) => {
                    *self.state.borrow_mut() = PlayState::Decoding(index, promise);
                    None
                }
            },
            other => {
                *self.state.borrow_mut() = other;
                None
            }
        };

        if let Some((index, result)) = taken {
            match result {
                // `toggle` already created the player before spawning this decode.
                Ok(decoded) => {
                    let played = PLAYER.with_borrow_mut(|player| {
                        player.as_mut().map(|player| player.play(decoded, false))
                    });
                    match played {
                        Some(Ok(())) => *self.state.borrow_mut() = PlayState::Playing(index),
                        Some(Err(error)) => *self.error.borrow_mut() = Some(error.to_string()),
                        None => {}
                    }
                }
                Err(error) => *self.error.borrow_mut() = Some(error.to_string()),
            }
        }

        let mut state = self.state.borrow_mut();
        let still_playing = PLAYER.with_borrow(|player| player.as_ref().is_some_and(Player::is_playing));
        if matches!(&*state, PlayState::Playing(_)) && !still_playing {
            *state = PlayState::Idle;
        }
    }

    fn poll_export(&self) {
        self.export
            .borrow_mut()
            .take_if(|promise| promise.try_get().is_some());
    }
}

impl Drop for Rendered {
    fn drop(&mut self) {
        PLAYER.with_borrow_mut(|player| {
            if let Some(player) = player.as_mut() {
                player.stop();
            }
        });
    }
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::OggVorbis => "Ogg Vorbis",
        Codec::Hca => "HCA",
        Codec::Mp3 => "MP3",
        Codec::MsAdpcm => "MS ADPCM",
        Codec::Atrac9 => "ATRAC9",
        Codec::Pcm => "PCM",
        Codec::Empty => "Empty",
        Codec::Unknown(_) => "Unknown",
    }
}
