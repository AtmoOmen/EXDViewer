//! The light a zone states for itself, and controls for what it leaves to the engine.
//!
//! A scene names an environment per part of itself: an `.envb` holding a timeline per weather, and
//! the `EnvLocation` instance the part is centered on, which in turn names the `.amb` holding the
//! ambient light as spherical harmonics over the day. Both are read at one time of day and go into
//! the ambient buffer the composite reads.
//!
//! Everything neither file states is a control rather than a number invented here: which sky the
//! frame stands under, where the key light comes from, and every field of the ambient entry past the
//! harmonics and their scale.

use std::io::Cursor;
use std::ops::RangeInclusive;

use anyhow::Result;
use egui::{Color32, RichText};
use glam::{Vec3, Vec4};
use ironworks::file::amb::{self, Ambient as AmbientFile, TRACK_COUNT};
use ironworks::file::envs::{Keyframe, Value};
use ironworks::file::{File, envb, layer};

use super::super::super::mdl::program;
use super::super::super::{link, section};
use crate::backend::Backend;
use crate::utils::TrackedPromise;

/// Seconds in a day.
const DAY: f32 = 86_400.0;

/// Every sky the game holds, which its own id indexes, and the first id it holds samples for.
const SKY_LIGHT: &str = "bgcommon/nature/sky/ambient/skylight.amb";
const FIRST_SKY: u16 = 1;

/// Where the key light stands until the user moves it.
const AZIMUTH: f32 = -50.0;
const ELEVATION: f32 = 55.0;

/// The set of an `.envb` weather holding the light the whole zone is lit by.
const GLOBAL_LIGHTING: u32 = 0;

/// One channel's harmonics, as a file states them.
type Channels = [[f32; 9]; 3];

/// A file the panel reads light out of.
enum Held<T> {
    Idle,
    Wanted(String),
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(T),
    Failed,
}

impl<T: File> Held<T> {
    fn poll(&mut self, backend: &Backend) {
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Wanted(path) => {
                let files = backend.files().clone();
                Self::Fetching(TrackedPromise::spawn_local(async move {
                    files.read(&path).await
                }))
            }
            Self::Fetching(promise) => match promise.try_get().map(|held| match held {
                Ok(bytes) => Ok(bytes.clone()),
                Err(why) => Err(why.to_string()),
            }) {
                Some(Ok(bytes)) => match T::read(Cursor::new(bytes)) {
                    Ok(held) => Self::Ready(held),
                    Err(why) => {
                        log::warn!("assets/layer: {why}");
                        Self::Failed
                    }
                },
                Some(Err(_)) => Self::Failed,
                None => Self::Fetching(promise),
            },
            held => held,
        };
    }

    fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(held) => Some(held),
            _ => None,
        }
    }

    fn pending(&self) -> bool {
        matches!(self, Self::Wanted(_) | Self::Fetching(_))
    }

    fn state(&self) -> &'static str {
        match self {
            Self::Ready(_) => "read",
            Self::Failed => "could not be read",
            Self::Idle => "none",
            _ => "loading",
        }
    }
}

/// One of the environments a scene applies over part of itself.
struct Environment {
    envb: String,
    /// The `EnvLocation` instance the environment is centered on, and the `.amb` it names once a
    /// walk has reached the layer group holding it.
    instance: u32,
    amb: Option<String>,
}

/// What one weather of an `.envb` states at one time of day.
#[derive(Default)]
struct Lighting {
    stated: bool,
    sunlight: Vec3,
    moonlight: Vec3,
    scale: f32,
    saturation: f32,
    attenuation: f32,
}

pub struct Ambient {
    environments: Vec<Environment>,
    at: usize,
    /// Which environment the loaded files belong to, so moving the picker fetches again.
    loaded: Option<usize>,
    weather_file: Held<envb::EnvironmentFile>,
    location: Held<AmbientFile>,
    sky_file: Held<AmbientFile>,

    /// Seconds since midnight.
    pub time: f32,
    /// Which of the `.amb`'s tracks the ambient is taken from. Nothing identifies what the index
    /// selects, so the count a file states per track is all there is to pick by.
    pub track: usize,
    pub weather: usize,
    /// Which sky the frame stands under, and nothing where the sky adds no light.
    pub sky: Option<u16>,
    pub sky_scale: f32,
    /// What the harmonics' three linear terms are weighted by. One is the file as it stands, nought
    /// leaves the ambient the same in every direction, and a negative turns it over.
    pub directionality: f32,
    /// Overrides the scale the `.envb` states.
    pub scale: Option<f32>,
    pub key: f32,
    pub azimuth: f32,
    pub elevation: f32,
    pub fade: Vec3,
    pub reflection: Vec3,
    pub roughness: f32,
    pub haze: Vec4,
}

impl Ambient {
    pub fn new(scene: Option<&layer::Scene>) -> Self {
        let environments = scene
            .map(|held| {
                held.environments()
                    .iter()
                    .filter(|env| !env.asset_path().is_empty())
                    .map(|env| Environment {
                        envb: env.asset_path().clone(),
                        instance: env.env_location_instance_id() as u32,
                        amb: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            environments,
            at: 0,
            loaded: None,
            weather_file: Held::Idle,
            location: Held::Idle,
            sky_file: Held::Idle,
            time: DAY / 2.0,
            track: 0,
            weather: 0,
            sky: None,
            sky_scale: 1.0,
            directionality: 1.0,
            scale: None,
            key: 1.0,
            azimuth: AZIMUTH,
            elevation: ELEVATION,
            fade: Vec3::new(0.0, 1.0, 0.0),
            reflection: Vec3::new(0.0, 1.0, 0.0),
            roughness: 0.0,
            haze: Vec4::W,
        }
    }

    /// The `.amb` an `EnvLocation` instance names, taken as a walk reaches it.
    pub fn locate(&mut self, instance: u32, path: &str) {
        if path.is_empty() {
            return;
        }
        for env in &mut self.environments {
            if env.instance == instance {
                env.amb = Some(path.to_owned());
            }
        }
    }

    pub fn pending(&self) -> bool {
        self.weather_file.pending() || self.location.pending() || self.sky_file.pending()
    }

    pub fn poll(&mut self, backend: &Backend) {
        if let Some(env) = self.environments.get(self.at) {
            if self.loaded != Some(self.at) {
                self.weather_file = Held::Wanted(env.envb.clone());
                self.location = Held::Idle;
                self.loaded = Some(self.at);
            }
            // The `.amb` is named by an instance rather than by the scene, so it only arrives once a
            // walk has reached the layer group holding it.
            if let (Held::Idle, Some(path)) = (&self.location, &env.amb) {
                self.location = Held::Wanted(path.clone());
            }
        }
        if self.sky.is_some() && matches!(self.sky_file, Held::Idle) {
            self.sky_file = Held::Wanted(SKY_LIGHT.to_owned());
        }
        self.weather_file.poll(backend);
        self.location.poll(backend);
        self.sky_file.poll(backend);
    }

    /// The tracks the location's `.amb` holds keyframes for, and how many each holds.
    fn tracks(&self) -> Vec<(usize, usize)> {
        let Some(AmbientFile::EnvLocation(held)) = self.location.ready() else {
            return Vec::new();
        };
        (0..TRACK_COUNT)
            .filter_map(|track| {
                let count = held.track(track)?.len();
                (count > 0).then_some((track, count))
            })
            .collect()
    }

    fn weathers(&self) -> Vec<u32> {
        self.weather_file
            .ready()
            .map(|held| {
                held.environments()
                    .weathers()
                    .iter()
                    .map(ironworks::file::envs::Weather::id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the `.envb` states for the weather and time the panel stands at.
    fn lighting(&self) -> Lighting {
        let mut out = Lighting {
            scale: 1.0,
            ..Default::default()
        };
        let Some(set) = self
            .weather_file
            .ready()
            .and_then(|held| held.environments().weathers().get(self.weather))
            .and_then(|weather| {
                weather
                    .sets()
                    .iter()
                    .find(|set| set.kind() == GLOBAL_LIGHTING)
            })
        else {
            return out;
        };
        let keyframes = set.keyframes();
        let times: Vec<f32> = keyframes.iter().map(Keyframe::time).collect();
        let Some((before, after, share)) = between(&times, self.time) else {
            return out;
        };
        let (before, after) = (&keyframes[before], &keyframes[after]);
        let read = |name: &str| {
            let held = |keyframe: &Keyframe| {
                keyframe
                    .fields()
                    .iter()
                    .find(|(field, _)| *field == name)
                    .map(|(_, value)| value.clone())
            };
            held(before).zip(held(after))
        };
        let color = |name: &str| match read(name) {
            Some((Value::Colour(first), Value::Colour(second))) => {
                let of = |held: ironworks::file::envs::Colour| {
                    Vec3::new(
                        f32::from(held.red()),
                        f32::from(held.green()),
                        f32::from(held.blue()),
                    ) / 255.0
                        * held.intensity()
                };
                of(first).lerp(of(second), share)
            }
            _ => Vec3::ZERO,
        };
        let scalar = |name: &str, fallback: f32| match read(name) {
            Some((Value::Float(first), Value::Float(second))) => first + (second - first) * share,
            _ => fallback,
        };
        out.stated = true;
        out.sunlight = color("sunlight_color");
        out.moonlight = color("moonlight_color");
        out.scale = scalar("ambient_light_scale", 1.0);
        out.saturation = scalar("ambient_light_saturation", 1.0);
        out.attenuation = scalar("ambient_attenuation", 0.0);
        out
    }

    /// The key light: which way it comes from, which nothing states, and the color the zone does.
    /// The sun and the moon go in together, since the graph runs one directional pass and a zone
    /// cross-fades the two over the day.
    pub fn light(&self) -> (Vec3, Vec3) {
        let held = self.lighting();
        let color = match held.stated {
            true => held.sunlight + held.moonlight,
            false => Vec3::ONE,
        };
        let (azimuth, elevation) = (self.azimuth.to_radians(), self.elevation.to_radians());
        let direction = Vec3::new(
            elevation.cos() * azimuth.sin(),
            elevation.sin(),
            elevation.cos() * azimuth.cos(),
        );
        (direction.normalize_or_zero(), color * self.key)
    }

    /// The ambient buffer as the files and the controls together decide it.
    pub fn scene(&self) -> program::Ambient {
        program::Ambient {
            sky: rows(self.sky_light(), self.directionality),
            sky_scale: self.sky_scale,
            light: rows(self.harmonics(), self.directionality),
            scale: self.scale.unwrap_or_else(|| self.lighting().scale),
            fade: self.fade,
            reflection: self.reflection,
            roughness: self.roughness,
            haze: self.haze,
        }
    }

    /// The location's ambient light at the time the panel stands at.
    fn harmonics(&self) -> Option<Channels> {
        let AmbientFile::EnvLocation(held) = self.location.ready()? else {
            return None;
        };
        let keyframes = held.track(self.track)?;
        let times: Vec<f32> = keyframes.iter().map(amb::Keyframe::time).collect();
        let (before, after, share) = between(&times, self.time)?;
        Some(blend(
            channels(keyframes[before].light()),
            channels(keyframes[after].light()),
            share,
        ))
    }

    /// The sky's own light at that time. The file holds no time per sample, so its samples are read
    /// as an even run over the day, which for the twenty-four of them the skies mostly carry is an
    /// hour apiece.
    fn sky_light(&self) -> Option<Channels> {
        let AmbientFile::SkyLight(held) = self.sky_file.ready()? else {
            return None;
        };
        let samples = held.samples(self.sky?)?;
        if samples.is_empty() {
            return None;
        }
        let step = DAY / samples.len() as f32;
        let times: Vec<f32> = (0..samples.len()).map(|at| at as f32 * step).collect();
        let (before, after, share) = between(&times, self.time)?;
        Some(blend(
            channels(samples[before]),
            channels(samples[after]),
            share,
        ))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, follow: &mut Option<String>) -> bool {
        let mut changed = false;
        section(ui, "Environment");
        if self.environments.len() > 1 {
            ui.label(RichText::new("Environment").weak());
            egui::ComboBox::from_id_salt("scene_environment")
                .selected_text(format!("{} of {}", self.at + 1, self.environments.len()))
                .show_ui(ui, |ui| {
                    for at in 0..self.environments.len() {
                        changed |= ui
                            .selectable_value(&mut self.at, at, format!("{}", at + 1))
                            .changed();
                    }
                });
        }

        let hours = self.time / 3600.0;
        ui.label(
            RichText::new(format!(
                "Time of day  {:02}:{:02}",
                hours as u32,
                (hours.fract() * 60.0) as u32
            ))
            .weak(),
        );
        changed |= ui
            .add(egui::Slider::new(&mut self.time, 0.0..=DAY).show_value(false))
            .changed();

        let weathers = self.weathers();
        if !weathers.is_empty() {
            ui.label(RichText::new("Weather").weak());
            egui::ComboBox::from_id_salt("scene_weather")
                .selected_text(
                    weathers
                        .get(self.weather)
                        .map_or_else(String::new, u32::to_string),
                )
                .show_ui(ui, |ui| {
                    for (at, id) in weathers.iter().enumerate() {
                        changed |= ui
                            .selectable_value(&mut self.weather, at, id.to_string())
                            .changed();
                    }
                });
        }

        let tracks = self.tracks();
        if !tracks.is_empty() {
            ui.label(RichText::new("Ambient track").weak());
            egui::ComboBox::from_id_salt("scene_track")
                .selected_text(self.track.to_string())
                .show_ui(ui, |ui| {
                    for (track, count) in &tracks {
                        changed |= ui
                            .selectable_value(
                                &mut self.track,
                                *track,
                                format!("{track}  ({count})"),
                            )
                            .on_hover_text("what the index selects is unidentified")
                            .changed();
                    }
                });
        }

        ui.label(RichText::new("Sky").weak());
        ui.horizontal(|ui| {
            let mut lit = self.sky.is_some();
            if ui.checkbox(&mut lit, "").changed() {
                self.sky = lit.then_some(FIRST_SKY);
                changed = true;
            }
            let mut id = self.sky.unwrap_or(FIRST_SKY);
            if ui
                .add_enabled(lit, egui::DragValue::new(&mut id).range(FIRST_SKY..=599))
                .on_hover_text("no file says which sky a zone stands under")
                .changed()
            {
                self.sky = Some(id);
                changed = true;
            }
        });

        ui.add_space(6.0);
        ui.label(RichText::new("Ambient scale").weak());
        let mut scale = self.scale.unwrap_or_else(|| self.lighting().scale);
        ui.horizontal(|ui| {
            if ui.add(egui::Slider::new(&mut scale, 0.0..=4.0)).changed() {
                self.scale = Some(scale);
                changed = true;
            }
            if ui
                .small_button("file")
                .on_hover_text("take the scale the envb states")
                .clicked()
            {
                self.scale = None;
                changed = true;
            }
        });
        changed |= slider(
            ui,
            "Ambient directionality",
            &mut self.directionality,
            -1.0..=2.0,
        );
        changed |= slider(ui, "Sky harmonic scale", &mut self.sky_scale, 0.0..=8.0);
        changed |= slider(ui, "Key light", &mut self.key, 0.0..=4.0);
        changed |= slider(ui, "Key azimuth", &mut self.azimuth, -180.0..=180.0);
        changed |= slider(ui, "Key elevation", &mut self.elevation, -90.0..=90.0);
        changed |= slider(ui, "Reflection roughness", &mut self.roughness, 0.0..=1.0);

        ui.add_space(6.0);
        for (label, held) in [
            ("Depth fade: scale, bias, floor", &mut self.fade),
            ("Reflection: scale, bias, mix", &mut self.reflection),
        ] {
            ui.label(RichText::new(label).weak());
            for lane in 0..3 {
                changed |= ui
                    .add(egui::Slider::new(&mut held[lane], -1.0..=2.0))
                    .changed();
            }
        }

        ui.label(RichText::new("What the ambient mixes toward").weak());
        let mut haze = Color32::from_rgb(
            (self.haze.x.clamp(0.0, 1.0) * 255.0) as u8,
            (self.haze.y.clamp(0.0, 1.0) * 255.0) as u8,
            (self.haze.z.clamp(0.0, 1.0) * 255.0) as u8,
        );
        ui.horizontal(|ui| {
            if ui.color_edit_button_srgba(&mut haze).changed() {
                self.haze.x = f32::from(haze.r()) / 255.0;
                self.haze.y = f32::from(haze.g()) / 255.0;
                self.haze.z = f32::from(haze.b()) / 255.0;
                changed = true;
            }
            changed |= ui
                .add(egui::Slider::new(&mut self.haze.w, 0.0..=2.0))
                .changed();
        });

        ui.add_space(8.0);
        let env = self.environments.get(self.at);
        let files = [
            (
                "Ambient",
                self.location.state(),
                env.and_then(|held| held.amb.clone()),
            ),
            (
                "Weather",
                self.weather_file.state(),
                env.map(|held| held.envb.clone()),
            ),
        ];
        let held = self.lighting();
        let mut rows = Vec::new();
        if held.stated {
            let spell = |held: Vec3| format!("{:.3}, {:.3}, {:.3}", held.x, held.y, held.z);
            rows.extend([
                ("Sunlight", spell(held.sunlight)),
                ("Moonlight", spell(held.moonlight)),
                ("Ambient light scale", format!("{:.3}", held.scale)),
                ("Saturation", format!("{:.3}", held.saturation)),
                ("Attenuation", format!("{:.3}", held.attenuation)),
            ]);
        }
        egui::Grid::new("scene_environment_files")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (label, state, path) in files {
                    ui.label(RichText::new(label).weak());
                    match &path {
                        Some(path) => {
                            if link(ui, path, path) {
                                *follow = Some(path.clone());
                            }
                        }
                        None => {
                            ui.label(RichText::new(state).monospace());
                        }
                    }
                    ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                    ui.end_row();
                }
                for (label, value) in &rows {
                    ui.label(RichText::new(*label).weak());
                    ui.label(RichText::new(value).monospace());
                    ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                    ui.end_row();
                }
            });
        changed
    }
}

/// The three rows one set of harmonics reaches the shader as, with the linear terms weighted.
fn rows(held: Option<Channels>, directionality: f32) -> [Vec4; 3] {
    let Some(held) = held else {
        return [Vec4::ZERO; 3];
    };
    held.map(|channel| {
        let row = program::Ambient::row(&channel);
        row * Vec4::new(directionality, directionality, directionality, 1.0)
    })
}

fn channels(held: amb::Harmonics) -> Channels {
    [held.red(), held.green(), held.blue()]
}

fn blend(first: Channels, second: Channels, share: f32) -> Channels {
    std::array::from_fn(|channel| {
        std::array::from_fn(|at| {
            first[channel][at] + (second[channel][at] - first[channel][at]) * share
        })
    })
}

/// Where `time` falls among keyframes ascending by time: the two either side of it and how far it
/// stands between them. The day wraps, so midnight sits between the evening and the morning rather
/// than at one end of the run.
fn between(times: &[f32], time: f32) -> Option<(usize, usize, f32)> {
    let last = times.len().checked_sub(1)?;
    if time < times[0] || time >= times[last] {
        let span = DAY - times[last] + times[0];
        let past = match time >= times[last] {
            true => time - times[last],
            false => DAY - times[last] + time,
        };
        return Some((last, 0, if span > 0.0 { past / span } else { 0.0 }));
    }
    let after = times.iter().position(|held| *held > time).unwrap_or(last);
    let span = times[after] - times[after - 1];
    Some((
        after - 1,
        after,
        match span > 0.0 {
            true => (time - times[after - 1]) / span,
            false => 0.0,
        },
    ))
}

/// One labelled slider over a field of the panel.
fn slider(ui: &mut egui::Ui, label: &str, held: &mut f32, range: RangeInclusive<f32>) -> bool {
    ui.label(RichText::new(label).weak());
    ui.add(egui::Slider::new(held, range)).changed()
}

#[cfg(test)]
mod test {
    use super::{DAY, between};

    #[test]
    fn a_time_falls_between_the_keyframes_either_side_of_it() {
        let times = [3600.0, 7200.0, 10800.0];
        assert_eq!(between(&times, 3600.0), Some((0, 1, 0.0)));
        assert_eq!(between(&times, 5400.0), Some((0, 1, 0.5)));
        assert_eq!(between(&times, 7200.0), Some((1, 2, 0.0)));
        assert_eq!(between(&[], 0.0), None);
    }

    #[test]
    fn the_day_wraps_past_the_last_keyframe() {
        let times = [3600.0, 10800.0];
        let (before, after, share) = between(&times, 0.0).unwrap();
        assert_eq!((before, after), (1, 0));
        assert!((share - (DAY - 10800.0) / (DAY - 10800.0 + 3600.0)).abs() < 1e-6);
        assert_eq!(
            between(&times, 10800.0).map(|held| (held.0, held.1)),
            Some((1, 0))
        );
    }
}
