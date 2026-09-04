//! The `.avfx` an emote's own timeline fires, drawn as the game's own particles.
//!
//! A firing is one command's own start, so a motion firing the same file eight times over its loop
//! runs eight of them at once, each on its own clock. None of the commands that start a loop state
//! a length, so what ends one is the length the file itself states.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use glam::{Mat4, Vec3, Vec4};
use ironworks::file::File as _;
use ironworks::file::avfx::Avfx;

use super::super::avfx::{self, Shaders, Textures, gpu, program, sim};
use crate::backend::Backend;
use crate::settings::AVFX_FRAME_RATE;
use crate::utils::TrackedPromise;

/// One firing to draw: the file, where it stands in the world, how far into its own run it is, and
/// the tint the command that started it states.
pub struct Fired {
    pub id: u64,
    pub path: String,
    pub at: Mat4,
    pub since: f32,
    pub tint: Vec4,
}

enum File {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Box<Held>),
    Failed,
}

/// A file parsed once, with the card-side geometry its model particles draw.
struct Held {
    effect: sim::Effect,
    particles: Arc<Mutex<gpu::Particles>>,
}

/// Every effect an emote is firing, kept across frames: the files parsed once each, the particles
/// each firing has run out, and the textures and packages they are all drawn through.
#[derive(Default)]
pub struct Effects {
    files: HashMap<String, File>,
    running: HashMap<u64, sim::State>,
    textures: Textures,
    shaders: Shaders,
}

impl Effects {
    /// Takes up whatever is firing this frame: asks for any file not in hand, steps each firing to
    /// where its own clock has reached, and forgets the ones no longer named.
    pub fn poll(&mut self, ctx: &egui::Context, backend: &Backend, fired: &[Fired]) {
        if fired.is_empty() && self.files.is_empty() {
            return;
        }
        self.shaders.poll(backend);
        for held in fired {
            self.files.entry(held.path.clone()).or_insert_with(|| {
                let files = backend.files().clone();
                let wanted = held.path.clone();
                File::Fetching(TrackedPromise::spawn_local(
                    async move { files.read(&wanted).await },
                ))
            });
        }
        for (path, file) in self.files.iter_mut() {
            let File::Fetching(promise) = file else {
                continue;
            };
            let Some(landed) = promise.try_get() else {
                continue;
            };
            *file = match landed
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(|bytes| {
                    Avfx::read(Cursor::new(bytes.clone())).map_err(|why| why.to_string())
                }) {
                Ok(read) => {
                    let mut effect = sim::Effect::read(&read);
                    // Nothing reads the models again once they are on the card: a particle already
                    // carries the index it draws.
                    let models = std::mem::take(&mut effect.models);
                    log::info!("assets/mdl: the emote fires {path}, {} frames", effect.length);
                    File::Ready(Box::new(Held {
                        effect,
                        particles: gpu::Particles::new(models),
                    }))
                }
                Err(why) => {
                    log::warn!("assets/mdl: {path}: {why}");
                    File::Failed
                }
            };
        }

        let wanted: Vec<String> = self
            .files
            .values()
            .filter_map(|file| match file {
                File::Ready(held) => Some(held.effect.textures.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        self.textures.poll(ctx, backend, &wanted);

        let rate = AVFX_FRAME_RATE.get(ctx);
        let Self { files, running, .. } = self;
        running.retain(|id, _| fired.iter().any(|held| held.id == *id));
        for held in fired {
            let Some(File::Ready(file)) = files.get(&held.path) else {
                continue;
            };
            let end = match file.effect.bounded {
                true => file.effect.length,
                false => sim::LONGEST,
            };
            let frame = (held.since * rate) as i32;
            file.effect
                .seek(running.entry(held.id).or_default(), frame.clamp(0, end));
        }
    }

    /// What to draw this frame, one entry per file however many firings it has: a draw is the
    /// file's own programs and geometry, so every firing of one goes into a single stream.
    pub fn frames(
        &self,
        fired: &[Fired],
        view: Mat4,
        projection: Mat4,
        size: (f32, f32),
        eye: Vec3,
    ) -> Vec<(Arc<Mutex<gpu::Particles>>, gpu::Frame)> {
        // A sprite is set into the screen's plane, which is what the camera's own axes are for.
        let axes = glam::Mat3::from_mat4(view).transpose();
        let (right, up) = (axes.x_axis, axes.y_axis);
        self.files
            .iter()
            .filter_map(|(path, file)| {
                let File::Ready(file) = file else {
                    return None;
                };
                let bound = self.textures.bound(&file.effect.textures);
                let drawn: Vec<sim::Drawn> = fired
                    .iter()
                    .filter(|held| held.path == *path)
                    .filter_map(|held| {
                        let state = self.running.get(&held.id)?;
                        let (scale, rotation, translation) = held.at.to_scale_rotation_translation();
                        let scale = scale.abs().max_element().max(0.001);
                        Some(
                            file.effect
                                .drawn(state)
                                .into_iter()
                                .map(move |item| item.placed(rotation, translation, scale, held.tint)),
                        )
                    })
                    .flatten()
                    .collect();
                let batches = avfx::batches(&file.effect, drawn, &bound, view, eye, right, up);
                (!batches.is_empty()).then(|| {
                    (
                        file.particles.clone(),
                        gpu::Frame {
                            scene: program::Scene {
                                view,
                                projection,
                                size,
                                light: (eye - Vec3::ZERO).normalize_or(Vec3::Y),
                                fade_range: file.effect.fade_range,
                                ..program::Scene::default()
                            },
                            batches,
                            packages: self.shaders.resolved(),
                            // Drawn after the character has been composited, which leaves no depth
                            // to test against and nothing to copy for the soft-particle variant.
                            tested: false,
                            depth: None,
                        },
                    )
                })
            })
            .collect()
    }
}
