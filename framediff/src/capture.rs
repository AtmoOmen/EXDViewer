//! Reads a converted RenderDoc capture: the frame the game presented, and the camera it drew under.
//!
//! `renderdoccmd convert -f x.rdc -c zip.xml -o x.zip` writes the XML beside a zip of blobs, and a
//! blob id is that zip's entry padded to six digits. The frame is the capture's own thumbnail, and
//! the camera is `g_CameraParameter` sitting in one of the heaps the frame wrote through.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3, Vec4};
use image::RgbImage;

use crate::View;

/// What the buffers must agree to before a camera is believed.
const TOLERANCE: f32 = 1e-4;

/// How far back from a projection matrix the view matrices beside it are looked for.
const WINDOW: usize = 1024;

pub struct Capture {
    pub name: String,
    pub xml: PathBuf,
    pub zip: PathBuf,
    /// What the swapchain presented, which the thumbnail is a resample of.
    pub extent: (u32, u32),
    pub thumbnail: (u32, u32),
    thumb: String,
    /// Blob ids of the heaps the frame wrote through, then of everything the capture kept.
    written: Vec<u32>,
    initial: Vec<u32>,
}

/// One camera a frame's buffers state, with how many copies of it they hold.
#[derive(Clone, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub forward: Vec3,
    /// Vertical, in degrees.
    pub fov: f32,
    pub aspect: f32,
    pub near: f32,
    pub copies: usize,
}

impl Camera {
    /// The frame as its own pixels, stretched to the shape it was drawn at rather than the shape a
    /// resampled thumbnail happens to be.
    pub fn view(&self, width: u32, height: u32) -> View {
        let mut held = View::of(self.eye, self.forward, self.fov, width, height);
        held.aspect = self.aspect;
        held
    }

    /// The angles a TitleEdit preset states, in radians.
    pub fn angles(&self) -> (f32, f32) {
        let held = self.forward.normalize_or(Vec3::NEG_Z);
        (held.x.atan2(held.z), held.y.clamp(-1.0, 1.0).asin())
    }
}

/// A direction the frame holds shaped like this engine's sun, and the hour that shape states.
#[derive(Clone, Debug)]
pub struct Sun {
    pub world: Vec3,
    pub hour: f32,
    /// Whether the buffer held it in the world or in the camera's own space.
    pub viewed: bool,
    pub copies: usize,
}

impl Capture {
    /// Opens a capture by either of the two files `convert` writes.
    pub fn open(path: &Path) -> Result<Self, String> {
        let text = path.to_string_lossy();
        let stem = text
            .strip_suffix(".zip.xml")
            .or_else(|| text.strip_suffix(".zip"))
            .ok_or_else(|| format!("{text}: not a converted capture"))?;
        let (xml, zip) = (
            PathBuf::from(format!("{stem}.zip.xml")),
            PathBuf::from(format!("{stem}.zip")),
        );
        for held in [&xml, &zip] {
            if !held.exists() {
                return Err(format!("{}: missing", held.display()));
            }
        }
        let mut held = Self {
            name: Path::new(stem)
                .file_name()
                .map_or_else(|| stem.to_owned(), |held| held.to_string_lossy().into_owned()),
            xml,
            zip,
            extent: (0, 0),
            thumbnail: (0, 0),
            thumb: "thumb.jpg".to_owned(),
            written: Vec::new(),
            initial: Vec::new(),
        };
        held.read_xml()?;
        Ok(held)
    }

    fn read_xml(&mut self) -> Result<(), String> {
        let file = File::open(&self.xml).map_err(|why| format!("{}: {why}", self.xml.display()))?;
        let mut lines = BufReader::with_capacity(1 << 20, file).lines();
        let mut chunk = String::new();
        let mut extent = false;
        let mut width = None;
        while let Some(Ok(line)) = lines.next() {
            if let Some(held) = between(&line, "<thumbnail width=\"", "\"") {
                self.thumbnail.0 = held.parse().unwrap_or(0);
                self.thumbnail.1 = between(&line, "height=\"", "\"")
                    .and_then(|held| held.parse().ok())
                    .unwrap_or(0);
                if let Some(held) = between(&line, "\">", "</thumbnail>") {
                    self.thumb = held.to_owned();
                }
            }
            if line.contains("<chunk id=") {
                chunk = between(&line, "name=\"", "\"").unwrap_or("").to_owned();
                extent = false;
            }
            if chunk == "vkCreateSwapchainKHR" {
                if line.contains("name=\"imageExtent\"") {
                    extent = true;
                    width = None;
                } else if extent && self.extent == (0, 0) {
                    if let Some(held) = between(&line, "name=\"width\"", "</uint>") {
                        width = held.rsplit('>').next().and_then(|held| held.parse().ok());
                    } else if let Some(held) = between(&line, "name=\"height\"", "</uint>") {
                        let height = held.rsplit('>').next().and_then(|held| held.parse().ok());
                        if let (Some(width), Some(height)) = (width, height) {
                            self.extent = (width, height);
                        }
                    }
                }
            }
            let held = match chunk.as_str() {
                "Internal::Coherent Mapped Memory Write" => &mut self.written,
                "Internal::Initial Contents" => &mut self.initial,
                _ => continue,
            };
            if let Some((open, _)) = line.split_once("</buffer>")
                && open.contains("<buffer ")
                && let Some(id) = open.rsplit('>').next()
                && let Ok(id) = id.parse()
            {
                held.push(id);
            }
        }
        Ok(())
    }

    /// The frame as it was presented, decoded from the capture's own thumbnail.
    pub fn frame(&self) -> Result<RgbImage, String> {
        let mut zip = self.archive()?;
        let mut held = zip
            .by_name(&self.thumb)
            .map_err(|why| format!("{}: {why}", self.thumb))?;
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes)
            .map_err(|why| why.to_string())?;
        crate::decode(&bytes)
    }

    fn archive(&self) -> Result<zip::ZipArchive<File>, String> {
        let file = File::open(&self.zip).map_err(|why| format!("{}: {why}", self.zip.display()))?;
        zip::ZipArchive::new(file).map_err(|why| format!("{}: {why}", self.zip.display()))
    }

    /// Every camera the frame's own writes state, most copies first.
    pub fn cameras(&self) -> Result<Vec<Camera>, String> {
        let mut held = self.cameras_in(&self.written)?;
        if held.is_empty() {
            held = self.cameras_in(&self.initial)?;
        }
        held.sort_by_key(|held| std::cmp::Reverse(held.copies));
        Ok(held)
    }

    /// The hours the frame's own light directions state. This engine's sun runs
    /// `(cos t, sin t/sqrt2, sin t/sqrt2)` with `t = 15 deg * (hour - 6)`, so any unit direction
    /// whose y and z are equal names an hour. More than one is stated and which of them is the sun
    /// is not something the buffers say, so these are evidence rather than an answer.
    pub fn suns(&self, camera: &Camera) -> Result<Vec<Sun>, String> {
        let mut zip = self.archive()?;
        let view = camera.view(1, 1).rotation.transpose();
        let mut found: Vec<Sun> = Vec::new();
        for id in &self.written {
            let Ok(mut entry) = zip.by_name(&format!("{id:06}")) else {
                continue;
            };
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            if entry.read_to_end(&mut bytes).is_err() {
                continue;
            }
            for held in suns(&bytes, view) {
                match found
                    .iter_mut()
                    .find(|other| other.viewed == held.viewed && (other.hour - held.hour).abs() < 1e-3)
                {
                    Some(other) => other.copies += 1,
                    None => found.push(held),
                }
            }
        }
        found.sort_by_key(|held| std::cmp::Reverse(held.copies));
        Ok(found)
    }

    fn cameras_in(&self, blobs: &[u32]) -> Result<Vec<Camera>, String> {
        let mut zip = self.archive()?;
        let mut found: Vec<Camera> = Vec::new();
        let aspect = self.extent.0 as f32 / self.extent.1.max(1) as f32;
        for id in blobs {
            let name = format!("{id:06}");
            let Ok(mut entry) = zip.by_name(&name) else {
                continue;
            };
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            if entry.read_to_end(&mut bytes).is_err() {
                continue;
            }
            for held in cameras(&bytes, aspect) {
                match found
                    .iter_mut()
                    .find(|other| other.eye.abs_diff_eq(held.eye, 1e-3) && other.fov == held.fov)
                {
                    Some(other) => other.copies += 1,
                    None => found.push(held),
                }
            }
        }
        Ok(found)
    }
}

fn between<'a>(line: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let held = line.split_once(open)?.1;
    held.split_once(close).map(|(held, _)| held)
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|held| f32::from_le_bytes([held[0], held[1], held[2], held[3]]))
        .collect()
}

fn row(held: &[f32], at: usize) -> Vec4 {
    Vec4::new(held[at], held[at + 1], held[at + 2], held[at + 3])
}

fn from_rows(rows: [Vec4; 4]) -> Mat4 {
    Mat4::from_cols(rows[0], rows[1], rows[2], rows[3]).transpose()
}

/// A projection this engine writes: a perspective with an infinite far plane, read as rows.
fn projection(held: &[f32], at: usize, aspect: f32) -> Option<(Mat4, f32, f32)> {
    let rows: [Vec4; 4] = std::array::from_fn(|step| row(held, at + step * 4));
    let zero = |value: f32| value == 0.0;
    let sparse = zero(rows[0].y)
        && zero(rows[0].z)
        && zero(rows[0].w)
        && zero(rows[1].x)
        && zero(rows[1].z)
        && zero(rows[1].w)
        && zero(rows[2].x)
        && zero(rows[2].y)
        && zero(rows[2].z)
        && zero(rows[3].x)
        && zero(rows[3].y)
        && zero(rows[3].w);
    if !sparse || rows[0].x.abs() < 1e-3 || rows[1].y.abs() < 1e-3 || rows[2].w <= 0.0 {
        return None;
    }
    // The pair reads as a lens only one way round: the vertical term is the larger one on a frame
    // wider than it is tall, and their ratio is the frame's own shape.
    if (rows[1].y / rows[0].x - aspect).abs() > 1e-2 * aspect {
        return None;
    }
    Some((from_rows(rows), rows[1].y, rows[2].w))
}

/// A world-to-view transform, as three rows of a rotation and a place.
fn rigid(held: &[f32], at: usize) -> Option<Mat4> {
    let rows: [Vec4; 3] = std::array::from_fn(|step| row(held, at + step * 4));
    let unit = |held: Vec4| (held.truncate().length_squared() - 1.0).abs() < TOLERANCE;
    let square = |a: Vec4, b: Vec4| a.truncate().dot(b.truncate()).abs() < TOLERANCE;
    if !unit(rows[0])
        || !unit(rows[1])
        || !unit(rows[2])
        || !square(rows[0], rows[1])
        || !square(rows[0], rows[2])
        || !square(rows[1], rows[2])
    {
        return None;
    }
    Some(from_rows([rows[0], rows[1], rows[2], Vec4::W]))
}

/// Every camera a heap holds, found by the projection each was written beside. The pair is only
/// believed once the view-projection the engine wrote from them is found beside them too.
fn cameras(bytes: &[u8], aspect: f32) -> Vec<Camera> {
    let held = floats(bytes);
    let mut out = Vec::new();
    for at in (0..held.len().saturating_sub(15)).step_by(4) {
        let Some((projection, reach, near)) = projection(&held, at, aspect) else {
            continue;
        };
        let back = at.saturating_sub(WINDOW / 4);
        let Some((view, eye)) = (back..at).step_by(4).rev().find_map(|step| {
            let view = rigid(&held, step)?;
            let inverse = rigid(&held, step + 12)?;
            if !(view * inverse).abs_diff_eq(Mat4::IDENTITY, TOLERANCE) {
                return None;
            }
            let want = projection * view;
            let written = (back..at).step_by(4).any(|other| {
                let rows: [Vec4; 4] = std::array::from_fn(|part| row(&held, other + part * 4));
                from_rows(rows).abs_diff_eq(want, TOLERANCE)
            });
            written.then_some((view, inverse.w_axis.truncate()))
        }) else {
            continue;
        };
        out.push(Camera {
            eye,
            // View space looks down its own negative z, which is what the projection's w row states
            // by taking the view's third row negated.
            forward: -view.transpose().z_axis.truncate(),
            fov: 2.0 * (1.0 / reach).atan().to_degrees(),
            aspect,
            near,
            copies: 1,
        });
    }
    out
}

/// The hour a sun-shaped direction states, or nothing where it names no hour.
fn hour(held: Vec3) -> Option<f32> {
    let sunlike = (held.length_squared() - 1.0).abs() < 1e-5
        && (held.y - held.z).abs() < 1e-4
        && held.y.abs() > 1e-3;
    sunlike.then(|| {
        let turn = (held.y * std::f32::consts::SQRT_2).atan2(held.x).to_degrees();
        (6.0 + turn / 15.0).rem_euclid(24.0)
    })
}

fn suns(bytes: &[u8], view: glam::Mat3) -> Vec<Sun> {
    let held = floats(bytes);
    let mut out = Vec::new();
    for at in (0..held.len().saturating_sub(3)).step_by(4) {
        let raw = Vec3::new(held[at], held[at + 1], held[at + 2]);
        for (viewed, world) in [(false, raw), (true, view * raw)] {
            if let Some(hour) = hour(world) {
                out.push(Sun {
                    world,
                    hour,
                    viewed,
                    copies: 1,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod test {
    use glam::{Vec3, Vec4};

    use super::cameras;

    /// The camera buffer as the game writes one, laid out the way a real frame's was.
    #[test]
    fn a_camera_reads_out_of_the_buffer_the_engine_writes() {
        let view = [
            [0.815_854, 0.0, -0.578_258, 26.569_677_f32],
            [-0.107_990, 0.982_408, -0.152_361, -13.365_686],
            [0.568_085, 0.186_750, 0.801_501, -29.194_782],
        ];
        let inverse = [
            [0.815_854, -0.107_990, 0.568_085, -6.535_227_f32],
            [0.0, 0.982_407, 0.186_750, 18.582_674],
            [-0.578_258, -0.152_361, 0.801_501, 36.727_367],
        ];
        let projection = [
            [1.044_534, 0.0, 0.0, 0.0_f32],
            [0.0, 1.920_982, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.1],
            [0.0, 0.0, -1.0, 0.0],
        ];
        let rows = |held: &[[f32; 4]]| {
            let mut out = [Vec4::W; 4];
            for (at, row) in held.iter().enumerate() {
                out[at] = Vec4::from_array(*row);
            }
            super::from_rows(out)
        };
        let combined = rows(&projection) * rows(&view);
        let mut held: Vec<f32> = vec![0.0; 16];
        for row in [&view[..], &inverse[..]] {
            held.extend(row.iter().flatten());
        }
        held.extend(combined.transpose().to_cols_array());
        held.extend([0.0; 16]);
        held.extend(projection.iter().flatten());
        let bytes: Vec<u8> = held.iter().flat_map(|held| held.to_le_bytes()).collect();
        let found = cameras(&bytes, 2560.0 / 1392.0);
        assert_eq!(found.len(), 1);
        let held = &found[0];
        assert!(held.eye.abs_diff_eq(Vec3::new(-6.535_227, 18.582_674, 36.727_367), 1e-4));
        assert!((held.fov - 55.0).abs() < 0.01, "{}", held.fov);
        assert!((held.near - 0.1).abs() < 1e-6);
        // Looking down and away, which is the only reading the projection's own w row allows.
        assert!(held.forward.abs_diff_eq(Vec3::new(-0.568_085, -0.186_750, -0.801_501), 1e-4));
    }
}
