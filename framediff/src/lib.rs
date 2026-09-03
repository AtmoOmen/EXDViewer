//! Measures a rendered frame against a captured one.
//!
//! The two frames are stood on one pixel grid from the projections each was drawn under, so what is
//! reported is a difference in shading rather than in where the camera pointed. Saturation is
//! carried beside luminance throughout: a uniform gain moves luminance and leaves saturation alone,
//! and the frame's gain is an auto-exposure that moves with how much of the zone has loaded.

pub mod capture;
pub mod state;

use std::fmt::Write as _;
use std::path::Path;

use glam::{Mat3, Vec3};
use image::{Rgb, RgbImage};

/// Reads a frame. A capture's own thumbnail is a JPEG, and which decoder unpacks it is not a free
/// choice: `image`'s upsampler disagrees with libjpeg by up to 108 over a whole frame, which moves
/// what is being measured here. `jpeg-decoder` agrees with it to three.
pub fn open(path: &Path) -> Result<RgbImage, String> {
    let bytes = std::fs::read(path).map_err(|why| format!("{}: {why}", path.display()))?;
    decode(&bytes).map_err(|why| format!("{}: {why}", path.display()))
}

pub fn decode(bytes: &[u8]) -> Result<RgbImage, String> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return image::load_from_memory(bytes)
            .map(|held| held.to_rgb8())
            .map_err(|why| why.to_string());
    }
    let mut held = jpeg_decoder::Decoder::new(bytes);
    let pixels = held.decode().map_err(|why| why.to_string())?;
    let info = held.info().ok_or("no jpeg header")?;
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            RgbImage::from_raw(info.width.into(), info.height.into(), pixels)
                .ok_or_else(|| "short jpeg".to_owned())
        }
        held => Err(format!("{held:?}: not a colour jpeg")),
    }
}

/// Rec.709, over the 8-bit values as they are shown rather than a linear reconstruction.
pub const WEIGHTS: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// What counts as a clipped channel.
const CLIPPED: u8 = 254;

/// A rectangle in pixels, upper bound exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub fn of(image: &RgbImage) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: image.width(),
            y1: image.height(),
        }
    }

    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    pub fn holds(&self, x: u32, y: u32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn read(text: &str) -> Result<Self, String> {
        let held: Vec<u32> = text
            .split(',')
            .map(|part| part.trim().parse().map_err(|_| format!("{text}: not four numbers")))
            .collect::<Result<_, _>>()?;
        match held[..] {
            [x0, y0, x1, y1] if x1 > x0 && y1 > y0 => Ok(Self { x0, y0, x1, y1 }),
            _ => Err(format!("{text}: not four rising numbers")),
        }
    }
}

/// Which pixels of a frame a measurement is allowed to read.
pub struct Region {
    pub rect: Rect,
    pub masks: Vec<Rect>,
    /// Cleared where the other frame does not reach, so the two are measured over the same pixels.
    pub kept: Vec<bool>,
}

impl Region {
    pub fn new(rect: Rect, masks: Vec<Rect>) -> Self {
        let kept = vec![true; (rect.width() * rect.height()) as usize];
        let mut held = Self { rect, masks, kept };
        for y in held.rect.y0..held.rect.y1 {
            for x in held.rect.x0..held.rect.x1 {
                if held.masks.iter().any(|mask| mask.holds(x, y)) {
                    held.drop(x, y);
                }
            }
        }
        held
    }

    fn at(&self, x: u32, y: u32) -> usize {
        ((y - self.rect.y0) * self.rect.width() + (x - self.rect.x0)) as usize
    }

    pub fn drop(&mut self, x: u32, y: u32) {
        if self.rect.holds(x, y) {
            let at = self.at(x, y);
            self.kept[at] = false;
        }
    }

    pub fn holds(&self, x: u32, y: u32) -> bool {
        self.rect.holds(x, y) && self.kept[self.at(x, y)]
    }

    pub fn pixels(&self) -> usize {
        self.kept.iter().filter(|held| **held).count()
    }
}

pub fn luminance(pixel: Rgb<u8>) -> f64 {
    (0..3).map(|at| f64::from(pixel[at]) * WEIGHTS[at]).sum()
}

/// How far a pixel is from grey, as a share of its brightest channel. Invariant under a gain.
pub fn saturation(pixel: Rgb<u8>) -> f64 {
    let high = f64::from(*pixel.0.iter().max().unwrap());
    let low = f64::from(*pixel.0.iter().min().unwrap());
    match high > 0.0 {
        true => (high - low) / high,
        false => 0.0,
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub pixels: usize,
    pub rgb: [f64; 3],
    pub luminance: f64,
    pub saturation: f64,
    pub median: f64,
    pub p99: f64,
    /// Share of pixels with any channel at or above 254.
    pub clipped: f64,
    /// Share of pixels with every channel at 255.
    pub white: f64,
    /// Share of pixels with every channel at nought.
    pub black: f64,
}

impl Stats {
    pub fn of(image: &RgbImage, region: &Region) -> Self {
        let mut held = Self::default();
        let mut lums = Vec::with_capacity(region.rect.width() as usize * 8);
        for y in region.rect.y0..region.rect.y1 {
            for x in region.rect.x0..region.rect.x1 {
                if !region.holds(x, y) {
                    continue;
                }
                let pixel = *image.get_pixel(x, y);
                for at in 0..3 {
                    held.rgb[at] += f64::from(pixel[at]);
                }
                let lum = luminance(pixel);
                held.luminance += lum;
                held.saturation += saturation(pixel);
                held.clipped += f64::from(u8::from(pixel.0.iter().any(|held| *held >= CLIPPED)));
                held.white += f64::from(u8::from(pixel.0.iter().all(|held| *held == u8::MAX)));
                held.black += f64::from(u8::from(pixel.0.iter().all(|held| *held == 0)));
                lums.push(lum);
                held.pixels += 1;
            }
        }
        if held.pixels == 0 {
            return held;
        }
        let count = held.pixels as f64;
        for at in 0..3 {
            held.rgb[at] /= count;
        }
        held.luminance /= count;
        held.saturation /= count;
        held.clipped *= 100.0 / count;
        held.white *= 100.0 / count;
        held.black *= 100.0 / count;
        lums.sort_by(f64::total_cmp);
        held.median = quantile(&lums, 0.5);
        held.p99 = quantile(&lums, 0.99);
        held
    }

    pub fn row(&self) -> String {
        format!(
            "lum {:6.2}  sat {:6.4}  rgb ({:5.1}, {:5.1}, {:5.1})  med {:6.2}  p99 {:6.2}  \
             clip {:5.2}%  white {:5.2}%  black {:5.2}%",
            self.luminance,
            self.saturation,
            self.rgb[0],
            self.rgb[1],
            self.rgb[2],
            self.median,
            self.p99,
            self.clipped,
            self.white,
            self.black,
        )
    }
}

/// Linear interpolation between order statistics, as numpy's default.
fn quantile(sorted: &[f64], at: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let place = at * (sorted.len() - 1) as f64;
    let low = place.floor() as usize;
    let high = place.ceil() as usize;
    sorted[low] + (sorted[high] - sorted[low]) * (place - low as f64)
}

/// Where a camera stood and what it saw through, as both a capture and this viewer state one.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: Vec3,
    /// World to view, right handed, looking down negative z.
    pub rotation: Mat3,
    /// Vertical, in degrees.
    pub fov: f32,
    /// What the frame was drawn at, which a resampled thumbnail no longer states by its own shape.
    pub aspect: f32,
    pub width: u32,
    pub height: u32,
}

impl View {
    pub fn of(eye: Vec3, forward: Vec3, fov: f32, width: u32, height: u32) -> Self {
        let forward = forward.normalize_or(Vec3::NEG_Z);
        let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(forward);
        Self {
            eye,
            rotation: Mat3::from_cols(right, up, -forward).transpose(),
            fov,
            aspect: width as f32 / height as f32,
            width,
            height,
        }
    }

    pub fn forward(&self) -> Vec3 {
        -self.rotation.transpose().z_axis
    }

    /// Half the frame's height at unit depth.
    pub fn reach(&self) -> f32 {
        (self.fov.to_radians() * 0.5).tan()
    }

    /// The direction a pixel's centre looks, in this view's own space.
    fn ray(&self, x: f32, y: f32) -> Vec3 {
        let nx = 2.0 * (x + 0.5) / self.width as f32 - 1.0;
        let ny = 1.0 - 2.0 * (y + 0.5) / self.height as f32;
        Vec3::new(nx * self.reach() * self.aspect, ny * self.reach(), -1.0)
    }

    /// Where a direction in this view's space lands, as a pixel place.
    fn place(&self, ray: Vec3) -> Option<(f32, f32)> {
        if ray.z >= 0.0 {
            return None;
        }
        let nx = ray.x / (-ray.z * self.reach() * self.aspect);
        let ny = ray.y / (-ray.z * self.reach());
        Some((
            (nx + 1.0) * 0.5 * self.width as f32 - 0.5,
            (1.0 - ny) * 0.5 * self.height as f32 - 0.5,
        ))
    }
}

/// How far apart two views stand. A turn and a lens can be resampled away; a step cannot, because
/// what it moves is parallax.
#[derive(Clone, Copy, Debug)]
pub struct Residual {
    pub step: f32,
    /// Between the two forward directions, in degrees.
    pub turn: f32,
    /// Roll about the forward axis, in degrees.
    pub roll: f32,
    pub fov: f32,
    /// The step as an angle at a hundred units, which is what it costs a frame of that reach.
    pub parallax: f32,
    /// What the turn is worth on the game's own pixel grid.
    pub pixels: f32,
}

impl Residual {
    pub fn between(game: &View, view: &View) -> Self {
        let step = (view.eye - game.eye).length();
        let turn = game
            .forward()
            .dot(view.forward())
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees();
        let between = view.rotation * game.rotation.transpose();
        let roll = between.y_axis.x.atan2(between.y_axis.y).to_degrees();
        Self {
            step,
            turn,
            roll,
            fov: view.fov - game.fov,
            parallax: (step / 100.0).atan().to_degrees(),
            pixels: turn.to_radians() * 0.5 * game.height as f32 / game.reach(),
        }
    }
}

/// One frame resampled onto the other's pixel grid, and which of those pixels it reached.
pub struct Aligned {
    pub image: RgbImage,
    pub outside: Vec<bool>,
}

/// Resamples `image` from `view` onto `onto`'s grid, taking every pixel through the direction it
/// looks rather than through a fitted scale.
pub fn align(image: &RgbImage, view: &View, onto: &View) -> Aligned {
    let between = view.rotation * onto.rotation.transpose();
    let mut out = RgbImage::new(onto.width, onto.height);
    let mut outside = vec![false; (onto.width * onto.height) as usize];
    for y in 0..onto.height {
        for x in 0..onto.width {
            let ray = between * onto.ray(x as f32, y as f32);
            let at = (y * onto.width + x) as usize;
            match view.place(ray) {
                Some((sx, sy)) if reaches(image, sx, sy) => {
                    out.put_pixel(x, y, sample(image, sx, sy));
                }
                _ => outside[at] = true,
            }
        }
    }
    Aligned {
        image: out,
        outside,
    }
}

fn reaches(image: &RgbImage, x: f32, y: f32) -> bool {
    x >= 0.0 && y >= 0.0 && x <= (image.width() - 1) as f32 && y <= (image.height() - 1) as f32
}

fn sample(image: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = (
        (x0 + 1).min(image.width() - 1),
        (y0 + 1).min(image.height() - 1),
    );
    let (fx, fy) = (f64::from(x - x0 as f32), f64::from(y - y0 as f32));
    let mut out = [0u8; 3];
    for (at, held) in out.iter_mut().enumerate() {
        let top = f64::from(image.get_pixel(x0, y0)[at]) * (1.0 - fx)
            + f64::from(image.get_pixel(x1, y0)[at]) * fx;
        let bottom = f64::from(image.get_pixel(x0, y1)[at]) * (1.0 - fx)
            + f64::from(image.get_pixel(x1, y1)[at]) * fx;
        *held = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

/// One cell of the grid a frame is reported over.
pub struct Cell {
    pub rect: Rect,
    pub game: Stats,
    pub view: Stats,
}

impl Cell {
    /// How much brighter the game is, which a shared gain leaves at one.
    pub fn gain(&self) -> f64 {
        match self.view.luminance > 0.0 {
            true => self.game.luminance / self.view.luminance,
            false => f64::NAN,
        }
    }

    /// How much more colourful this frame is than the game, which a gain does not move.
    pub fn tint(&self) -> f64 {
        self.view.saturation - self.game.saturation
    }
}

pub fn grid(
    game: &RgbImage,
    view: &RgbImage,
    region: &Region,
    across: u32,
    down: u32,
) -> Vec<Vec<Cell>> {
    let rect = region.rect;
    (0..down)
        .map(|row| {
            (0..across)
                .map(|column| {
                    let cell = Rect {
                        x0: rect.x0 + rect.width() * column / across,
                        y0: rect.y0 + rect.height() * row / down,
                        x1: rect.x0 + rect.width() * (column + 1) / across,
                        y1: rect.y0 + rect.height() * (row + 1) / down,
                    };
                    let mut held = Region::new(cell, Vec::new());
                    for y in cell.y0..cell.y1 {
                        for x in cell.x0..cell.x1 {
                            if !region.holds(x, y) {
                                held.drop(x, y);
                            }
                        }
                    }
                    Cell {
                        rect: cell,
                        game: Stats::of(game, &held),
                        view: Stats::of(view, &held),
                    }
                })
                .collect()
        })
        .collect()
}

/// The two frames side by side per cell, brightest disagreement first.
pub fn worst(cells: &[Vec<Cell>], keep: usize) -> String {
    let mut held: Vec<&Cell> = cells
        .iter()
        .flatten()
        .filter(|cell| cell.game.pixels > 64)
        .collect();
    held.sort_by(|a, b| {
        let score = |cell: &Cell| (cell.gain().ln().abs()).max(cell.tint().abs() * 4.0);
        score(b).total_cmp(&score(a))
    });
    let mut out = String::new();
    for cell in held.into_iter().take(keep) {
        let _ = writeln!(
            out,
            "  ({:4},{:4})-({:4},{:4})  gain {:5.2}  sat {:+.3} (game {:.3}, here {:.3})  \
             lum game {:6.2} here {:6.2}",
            cell.rect.x0,
            cell.rect.y0,
            cell.rect.x1,
            cell.rect.y1,
            cell.gain(),
            cell.tint(),
            cell.game.saturation,
            cell.view.saturation,
            cell.game.luminance,
            cell.view.luminance,
        );
    }
    out
}

/// The two frames on one image, the game as red and this one as green. Where they agree the frame
/// is grey; a structure in one colour alone is geometry only one of them drew, and a wash of one is
/// a difference in brightness. This is what says whether a diff is measuring shading or aim.
pub fn overlay(game: &RgbImage, view: &RgbImage, region: &Region) -> RgbImage {
    let mut out = RgbImage::new(game.width(), game.height());
    for y in 0..game.height() {
        for x in 0..game.width() {
            if !region.holds(x, y) {
                continue;
            }
            let held = |image: &RgbImage| luminance(*image.get_pixel(x, y)).round() as u8;
            out.put_pixel(x, y, Rgb([held(game), held(view), 0]));
        }
    }
    out
}

/// The difference between two frames as an image, scaled so a small one is visible.
pub fn difference(game: &RgbImage, view: &RgbImage, region: &Region, gain: f32) -> RgbImage {
    let mut out = RgbImage::new(game.width(), game.height());
    for y in 0..game.height() {
        for x in 0..game.width() {
            if !region.holds(x, y) {
                continue;
            }
            let (a, b) = (game.get_pixel(x, y), view.get_pixel(x, y));
            let mut held = [0u8; 3];
            for at in 0..3 {
                let apart = (f32::from(a[at]) - f32::from(b[at])).abs() * gain;
                held[at] = apart.min(255.0) as u8;
            }
            out.put_pixel(x, y, Rgb(held));
        }
    }
    out
}

#[cfg(test)]
mod test {
    use glam::Vec3;
    use image::{Rgb, RgbImage};

    use super::{Rect, Region, Residual, Stats, View, align, luminance, saturation};

    #[test]
    fn a_grey_pixel_carries_no_saturation_and_a_gain_does_not_move_one() {
        assert_eq!(saturation(Rgb([80, 80, 80])), 0.0);
        assert_eq!(saturation(Rgb([0, 0, 0])), 0.0);
        let (dim, bright) = (Rgb([20, 10, 5]), Rgb([80, 40, 20]));
        assert!((saturation(dim) - saturation(bright)).abs() < 1e-12);
        assert!((luminance(bright) / luminance(dim) - 4.0).abs() < 1e-12);
    }

    #[test]
    fn a_mask_is_left_out_of_what_is_measured() {
        let mut image = RgbImage::new(4, 2);
        for x in 0..4 {
            image.put_pixel(x, 0, Rgb([255, 255, 255]));
        }
        let whole = Stats::of(&image, &Region::new(Rect::of(&image), Vec::new()));
        assert_eq!(whole.pixels, 8);
        assert_eq!(whole.white, 50.0);
        assert_eq!(whole.black, 50.0);
        let masked = Region::new(
            Rect::of(&image),
            vec![Rect {
                x0: 0,
                y0: 0,
                x1: 4,
                y1: 1,
            }],
        );
        assert_eq!(Stats::of(&image, &masked).pixels, 4);
        assert_eq!(Stats::of(&image, &masked).white, 0.0);
        assert_eq!(Stats::of(&image, &masked).black, 100.0);
    }

    #[test]
    fn the_median_and_the_percentile_read_as_numpy_does() {
        let mut image = RgbImage::new(101, 1);
        for x in 0..101u32 {
            let held = x as u8;
            image.put_pixel(x, 0, Rgb([held, held, held]));
        }
        let held = Stats::of(&image, &Region::new(Rect::of(&image), Vec::new()));
        assert!((held.median - 50.0).abs() < 1e-9);
        assert!((held.p99 - 99.0).abs() < 1e-9);
    }

    /// Two views of the same place through different lenses land the same world direction on the
    /// same pixel, which is what makes a resampled frame comparable.
    #[test]
    fn a_wider_lens_resamples_onto_a_narrower_one_without_moving_anything() {
        let eye = Vec3::new(3.0, 4.0, 5.0);
        let forward = Vec3::new(0.3, -0.2, -1.0);
        let game = View::of(eye, forward, 55.0, 64, 48);
        let view = View::of(eye, forward, 80.0, 96, 48);
        for (x, y) in [(0, 0), (31, 23), (63, 47), (10, 40)] {
            let ray = game.ray(x as f32, y as f32);
            let (sx, sy) = view.place(ray).expect("in front");
            let back = view.ray(sx, sy);
            assert!(back.normalize().dot(ray.normalize()) > 1.0 - 1e-6);
        }
    }

    #[test]
    fn a_resampled_frame_keeps_what_it_covers_and_marks_what_it_does_not() {
        let eye = Vec3::ZERO;
        let forward = Vec3::NEG_Z;
        let mut wide = RgbImage::new(64, 32);
        for pixel in wide.pixels_mut() {
            *pixel = Rgb([10, 20, 30]);
        }
        let held = align(
            &wide,
            &View::of(eye, forward, 40.0, 64, 32),
            &View::of(eye, forward, 90.0, 64, 32),
        );
        assert_eq!(*held.image.get_pixel(32, 16), Rgb([10, 20, 30]));
        assert!(held.outside[0], "a corner past the narrow frame");
        assert!(!held.outside[(16 * 64 + 32) as usize]);
    }

    #[test]
    fn a_step_and_a_turn_are_reported_apart() {
        let held = View::of(Vec3::ZERO, Vec3::NEG_Z, 55.0, 16, 9);
        let stepped = View::of(Vec3::new(0.0, 0.0, -2.0), Vec3::NEG_Z, 55.0, 16, 9);
        let turned = View::of(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0).normalize(), 60.0, 16, 9);
        let apart = Residual::between(&held, &stepped);
        assert!((apart.step - 2.0).abs() < 1e-5 && apart.turn < 1e-3);
        let apart = Residual::between(&held, &turned);
        assert!(apart.step < 1e-6 && (apart.fov - 5.0).abs() < 1e-5);
    }
}
