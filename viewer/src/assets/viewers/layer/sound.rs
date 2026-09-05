//! Every `Sound` instance a scene's layer groups place, flattened out of the tree and playable
//! where the entry's codec is one `audio::decode_data` supports.
//!
//! A zone's own scene section never carries a `Sound` instance directly: every one of them lives
//! in an externally named `.lgb` (typically `sound.lgb`), so those are fetched here rather than
//! only reading what the file already held in memory.

use std::time::Duration;

use egui::{Color32, Label, RichText, ScrollArea, Sense};
use ironworks::file::layer::{InstanceData, LayerGroup, SoundEffectKind};
use ironworks::file::lgb::LayerGroupFile;
use ironworks::file::scd::SoundContainer;

use super::Source;
use crate::assets::path_context;
use crate::assets::viewers::{headers, section};
use crate::audio::{self, Player};
use crate::backend::Backend;
use crate::data::FileProviderExt;
use crate::utils::{PromiseKind, TrackedPromise};

struct Placement {
    kind: SoundEffectKind,
    asset_path: String,
    auto_play: bool,
    no_far_clip: bool,
}

fn placements(groups: &[LayerGroup]) -> Vec<Placement> {
    groups
        .iter()
        .flat_map(LayerGroup::layers)
        .flat_map(|layer| layer.instances())
        .filter_map(|instance| match instance.data() {
            // The three obstruction kinds shape how another sound propagates through them rather
            // than naming one of their own, so they never carry an asset path and have nothing to
            // list here.
            InstanceData::Sound(sound) if !sound.asset_path().is_empty() => Some(Placement {
                kind: sound.kind(),
                asset_path: sound.asset_path().clone(),
                auto_play: sound.auto_play(),
                no_far_clip: sound.no_far_clip(),
            }),
            _ => None,
        })
        .collect()
}

enum External {
    Pending(Vec<String>),
    Loading(TrackedPromise<Vec<Placement>>),
    Done,
}

enum PlayState {
    Idle,
    Decoding(usize, TrackedPromise<anyhow::Result<audio::Decoded>>),
    Playing(usize),
}

thread_local! {
    // One backend for every zone or layer group the viewer opens, not one per file, for the same
    // reason `scd.rs` keeps a single one: tearing an `AudioContext` down as another spins up races
    // its `onended` handler on the web.
    static PLAYER: std::cell::RefCell<Option<Player>> = const { std::cell::RefCell::new(None) };
}

pub struct Sounds {
    placements: Vec<Placement>,
    external: External,
    play: PlayState,
    error: Option<String>,
}

const COLUMNS: usize = 5;
const HEADERS: [&str; COLUMNS] = ["", "类型", "声音", "自动播放", "无远裁剪"];

impl Sounds {
    pub(super) fn new(source: &Source) -> Self {
        let external = source
            .scene()
            .map(|scene| scene.layer_group_paths().to_vec())
            .unwrap_or_default();
        Self {
            placements: placements(source.groups()),
            external: match external.is_empty() {
                true => External::Done,
                false => External::Pending(external),
            },
            play: PlayState::Idle,
            error: None,
        }
    }

    fn poll(&mut self, backend: &Backend) {
        if let External::Pending(paths) = &mut self.external {
            let paths = std::mem::take(paths);
            let files = backend.files().clone();
            self.external = External::Loading(TrackedPromise::spawn_local(async move {
                let mut found = Vec::new();
                for path in paths {
                    if let Ok(file) = files.file::<LayerGroupFile>(&path).await {
                        found.extend(placements(std::slice::from_ref(file.group())));
                    }
                }
                found
            }));
        }
        if matches!(&self.external, External::Loading(promise) if promise.try_get().is_some()) {
            let External::Loading(promise) = std::mem::replace(&mut self.external, External::Done)
            else {
                unreachable!()
            };
            self.placements.extend(promise.block_and_take());
        }

        let previous = std::mem::replace(&mut self.play, PlayState::Idle);
        self.play = match previous {
            PlayState::Decoding(index, promise) => match promise.try_take() {
                Ok(Ok(decoded)) => {
                    log::info!("assets/layer/sound: 已解码，播放索引 {index}");
                    let played = PLAYER
                        .with_borrow_mut(|player| player.as_mut().map(|p| p.play(decoded, false)));
                    match played {
                        Some(Ok(())) => PlayState::Playing(index),
                        Some(Err(error)) => {
                            log::info!("assets/layer/sound: 播放失败：{error}");
                            self.error = Some(error.to_string());
                            PlayState::Idle
                        }
                        None => {
                            log::info!("assets/layer/sound: 无播放器");
                            PlayState::Idle
                        }
                    }
                }
                Ok(Err(error)) => {
                    log::info!("assets/layer/sound: 解码失败：{error}");
                    self.error = Some(error.to_string());
                    PlayState::Idle
                }
                Err(promise) => PlayState::Decoding(index, promise),
            },
            other => other,
        };

        let still_playing =
            PLAYER.with_borrow(|player| player.as_ref().is_some_and(Player::is_playing));
        if matches!(self.play, PlayState::Playing(_)) && !still_playing {
            self.play = PlayState::Idle;
        }
    }

    fn toggle(&mut self, index: usize, backend: &Backend) {
        log::info!("assets/layer/sound: 切换索引 {index}");
        let already = matches!(
            &self.play,
            PlayState::Playing(playing) | PlayState::Decoding(playing, _) if *playing == index
        );
        self.error = None;
        PLAYER.with_borrow_mut(|player| {
            if let Some(player) = player.as_mut() {
                player.stop();
            }
        });
        if already {
            self.play = PlayState::Idle;
            return;
        }
        // The backend has to be created (and, on the web, its context resumed) inside the click
        // itself; a browser only grants that from a real user gesture.
        let unlocked: anyhow::Result<()> = PLAYER.with_borrow_mut(|player| {
            if player.is_none() {
                *player = Some(Player::new()?);
            }
            player.as_ref().unwrap().unlock();
            Ok(())
        });
        if let Err(error) = unlocked {
            self.error = Some(error.to_string());
            return;
        }

        let Some(path) = self.placements.get(index).map(|p| p.asset_path.clone()) else {
            return;
        };
        let files = backend.files().clone();
        let promise = TrackedPromise::spawn_local(async move {
            let container = files.file::<SoundContainer>(&path).await?;
            let entry = container
                .entries()
                .first()
                .ok_or_else(|| anyhow::anyhow!("{path}: 没有音频流"))?;
            audio::decode_data(entry.format(), entry.data())
        });
        self.play = PlayState::Decoding(index, promise);
    }

    fn row(&mut self, ui: &mut egui::Ui, index: usize, backend: &Backend, follow: &mut Option<String>) {
        let (kind, path, auto_play, no_far_clip) = {
            let placement = &self.placements[index];
            (
                placement.kind,
                placement.asset_path.clone(),
                placement.auto_play,
                placement.no_far_clip,
            )
        };
        let playing = matches!(self.play, PlayState::Playing(playing) if playing == index);
        let decoding = matches!(self.play, PlayState::Decoding(decoding, _) if decoding == index);
        let glyph = match (playing, decoding) {
            (true, _) => "⏹",
            (_, true) => "…",
            _ => "▶",
        };
        if ui.button(glyph).clicked() {
            self.toggle(index, backend);
        }
        ui.label(format!("{kind:?}"));
        // Plain, unwrapped: a `.truncate()` or `.wrap()` label reports its desired width as
        // whatever the column already is, so it can never grow one and would lock the column at
        // whatever the first frame happened to offer.
        let name = crate::utils::file_name(&path);
        let response = ui
            .add(
                Label::new(RichText::new(name).monospace().color(ui.visuals().hyperlink_color))
                    .sense(Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        path_context(&response, &path, None);
        if response.clicked() {
            *follow = Some(path);
        }
        ui.label(on(auto_play));
        ui.label(on(no_far_clip));
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
        ui.end_row();
    }
}

fn on(value: bool) -> &'static str {
    match value {
        true => "是",
        false => "否",
    }
}

impl Drop for Sounds {
    fn drop(&mut self) {
        PLAYER.with_borrow_mut(|player| {
            if let Some(player) = player.as_mut() {
                player.stop();
            }
        });
    }
}

pub fn ui(ui: &mut egui::Ui, sounds: &mut Sounds, backend: &Backend) -> Option<String> {
    sounds.poll(backend);
    if !matches!(sounds.play, PlayState::Idle) {
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }

    if matches!(sounds.external, External::Loading(_)) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("正在读取声音层组…");
        });
        ui.add_space(4.0);
    }

    if sounds.placements.is_empty() {
        crate::utils::empty_view(ui, "🔇", "未放置声音");
        return None;
    }

    let mut follow = None;
    section(ui, "声音");
    // The error, if any, is drawn inside the scroll area rather than after it: a vertical
    // `ScrollArea` with `auto_shrink(false)` claims all remaining height for itself, and anything
    // placed after it in the same `ui` is pushed past the bottom of the panel.
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        egui::Grid::new("layer_sounds")
            .num_columns(COLUMNS)
            .striped(true)
            .show(ui, |ui| {
                headers(ui, &HEADERS);
                for index in 0..sounds.placements.len() {
                    sounds.row(ui, index, backend, &mut follow);
                }
            });
        if let Some(error) = &sounds.error {
            ui.add_space(8.0);
            ui.colored_label(Color32::RED, error.as_str());
        }
    });
    follow
}
