//! TitleEdit presets, which stand a capture and this viewer in the same place.
//!
//! The plugin renders a real zone behind the title screen with no NPCs and no players in the frame,
//! and states everything about that frame in one file: which zone, where the camera stood, what it
//! looked at, the field of view, the weather and the hour. Reading one back puts this view where the
//! capture was taken from, which is what makes the two comparable.

use std::cell::RefCell;

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Seconds in a day, which the offset wraps into.
const DAY: f32 = 86_400.0;

/// How far in front of the camera the point it looks at is written, which the plugin states as a
/// place in the world rather than as a direction. Anything along the ray says the same thing.
const TOWARD: f32 = 10.0;

#[derive(Deserialize, Serialize)]
struct Point {
    #[serde(rename = "X")]
    x: f32,
    #[serde(rename = "Y")]
    y: f32,
    #[serde(rename = "Z")]
    z: f32,
}

#[derive(Deserialize, Serialize)]
struct File {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "TerritoryPath")]
    territory: String,
    #[serde(rename = "CameraPos")]
    camera: Point,
    #[serde(rename = "FixOnPos")]
    toward: Point,
    #[serde(rename = "FovY")]
    fov: Option<f32>,
    #[serde(rename = "WeatherId")]
    weather: Option<u32>,
    #[serde(rename = "TimeOffset")]
    time: Option<f32>,
}

/// One preset as this view uses it.
pub struct Preset {
    pub name: String,
    /// The level the plugin names, as the path this viewer opens it by.
    pub level: String,
    pub camera: Vec3,
    pub toward: Vec3,
    pub fov: Option<f32>,
    pub weather: Option<u32>,
    /// Seconds since midnight, where the file states an offset.
    pub time: Option<f32>,
}

impl Preset {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let held: File = serde_json::from_slice(bytes).map_err(|why| why.to_string())?;
        let stem = held
            .territory
            .rsplit('/')
            .next()
            .ok_or("the preset names no territory")?;
        Ok(Self {
            name: held.name.unwrap_or_else(|| stem.to_owned()),
            // The plugin states the path without its extension, and the stem twice over: the
            // directory the level sits in and the file itself go by the same name.
            level: format!("bg/{}.lvb", held.territory),
            camera: Vec3::new(held.camera.x, held.camera.y, held.camera.z),
            toward: Vec3::new(held.toward.x, held.toward.y, held.toward.z),
            fov: held.fov,
            weather: held.weather,
            // The offset reads as `hhmm` with the minutes allowed to run past sixty: 240 is 02:40,
            // 640 is 06:40 and 1985 is 19:85, which is 20:25. Measured against the sun three
            // captures carried, each to three decimal places of an hour.
            time: held.time.map(|held| {
                let held = held as i32;
                let (hours, minutes) = (held / 100, held % 100);
                (f32::from(hours as i16) * 3600.0 + f32::from(minutes as i16) * 60.0)
                    .rem_euclid(DAY)
            }),
        })
    }

    /// Which way the camera looks, as the two angles this view holds one by.
    pub fn angles(&self) -> (f32, f32) {
        let held = (self.toward - self.camera).normalize_or_zero();
        (held.x.atan2(held.z), held.y.clamp(-1.0, 1.0).asin())
    }

    /// The view as the plugin would have written it, so a place found here can be stood in again in
    /// the game and captured.
    pub fn of(
        level: &str,
        camera: Vec3,
        forward: Vec3,
        fov: f32,
        weather: Option<u32>,
        time: f32,
    ) -> Self {
        let stem = level.rsplit('/').next().unwrap_or(level);
        Self {
            name: stem.trim_end_matches(".lvb").to_owned(),
            level: level.to_owned(),
            camera,
            toward: camera + forward.normalize_or_zero() * TOWARD,
            fov: Some(fov),
            weather,
            time: Some(time),
        }
    }

    pub fn write(&self) -> Result<String, String> {
        let point = |held: Vec3| Point {
            x: held.x,
            y: held.y,
            z: held.z,
        };
        let held = File {
            name: Some(self.name.clone()),
            territory: self
                .level
                .trim_start_matches("bg/")
                .trim_end_matches(".lvb")
                .to_owned(),
            camera: point(self.camera),
            toward: point(self.toward),
            fov: self.fov,
            weather: self.weather,
            time: self.time.map(|held| {
                let minutes = (held / 60.0).round() as i32;
                (minutes / 60 * 100 + minutes % 60) as f32
            }),
        };
        serde_json::to_string_pretty(&held).map_err(|why| why.to_string())
    }
}

thread_local! {
    /// A preset waiting for the level it names to open. Opening one builds a scene of its own, so
    /// there is nowhere inside a scene for it to live across that.
    static PENDING: RefCell<Option<Preset>> = const { RefCell::new(None) };
}

/// Keeps a preset until the level it names has opened.
pub fn hold(held: Preset) {
    PENDING.with(|slot| *slot.borrow_mut() = Some(held));
}

/// The preset a scene was opened for, where it was opened for one.
pub fn taken(level: &str) -> Option<Preset> {
    PENDING.with(|slot| {
        let held = slot.borrow_mut().take()?;
        match held.level == level {
            true => Some(held),
            false => None,
        }
    })
}

#[cfg(test)]
mod test {
    use glam::Vec3;

    use super::Preset;

    /// The preset the user captured Ishgard from, which is the shape every one of them has.
    #[test]
    fn a_preset_reads_as_the_plugin_wrote_it() {
        let held = Preset::read(
            br#"{
                "Name": "TE_Ishgard",
                "TerritoryPath": "ex1/01_roc_r2/twn/r2t1/level/r2t1",
                "CameraPos": { "X": -251.920822, "Y": 8.874063, "Z": 166.831223 },
                "FixOnPos": { "X": -245.1769, "Y": 12.92161, "Z": 160.655716 },
                "FovY": 45.0,
                "WeatherId": 15,
                "TimeOffset": 1985
            }"#,
        )
        .expect("a preset");
        assert_eq!(held.level, "bg/ex1/01_roc_r2/twn/r2t1/level/r2t1.lvb");
        assert_eq!(held.weather, Some(15));
        // `hhmm` with the minutes running over: 1985 is 19:85, which is 20:25.
        assert_eq!(held.time, Some(20.0 * 3600.0 + 25.0 * 60.0));
        let (yaw, pitch) = held.angles();
        // It looks back toward positive x and slightly up, which is what the two points say.
        assert!(yaw > 0.0 && pitch > 0.0);
        assert!((pitch.to_degrees() - 23.0).abs() < 1.0);
    }

    /// One written out and read back is the same view, so a place found here can be stood in again
    /// in the game and captured from.
    #[test]
    fn a_view_written_out_comes_back_the_same_view() {
        let held = Preset::of(
            "bg/ex1/01_roc_r2/twn/r2t1/level/r2t1.lvb",
            Vec3::new(-251.9, 8.9, 166.8),
            Vec3::new(0.6, 0.4, -0.7),
            45.0,
            Some(15),
            9.0 * 3600.0 + 5.0 * 60.0,
        );
        let back = Preset::read(held.write().expect("written").as_bytes()).expect("read back");
        assert_eq!(back.level, held.level);
        assert_eq!(back.weather, Some(15));
        assert_eq!(back.fov, Some(45.0));
        assert!((back.time.unwrap() - held.time.unwrap()).abs() < 1.0);
        let (yaw, pitch) = back.angles();
        let (want_yaw, want_pitch) = held.angles();
        assert!((yaw - want_yaw).abs() < 1e-4 && (pitch - want_pitch).abs() < 1e-4);
    }
}
