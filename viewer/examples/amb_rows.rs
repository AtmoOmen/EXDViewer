//! The three rows a sky's harmonics reach the composite as, which is a fingerprint distinctive
//! enough to find the ambient buffer in a capture.
//!
//! `amb_rows <sky id> <hour>`

use ironworks::file::File;
use ironworks::file::amb::{self, Ambient as AmbientFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const SKY_LIGHT: &str = "bgcommon/nature/sky/ambient/skylight.amb";

/// The two factors a cosine convolution weighs the terms by.
const CONSTANT: f32 = 0.886_226_9;
const LINEAR: f32 = 1.023_326_7;

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let id: u16 = args
        .next()
        .and_then(|held| held.parse().ok())
        .expect("a sky id");
    let hour: f32 = args
        .next()
        .and_then(|held| held.parse().ok())
        .unwrap_or(12.0);

    let bytes: Vec<u8> = ironworks.file(SKY_LIGHT).expect("skylight");
    let AmbientFile::SkyLight(held) = AmbientFile::read(std::io::Cursor::new(bytes)).expect("amb")
    else {
        panic!("skylight is not a sky light file");
    };
    let samples = held.samples(id).expect("that sky");
    // The file holds no time per sample, so they run evenly over the day.
    let step = 24.0 / samples.len() as f32;
    let at = (hour / step).floor() as usize % samples.len();
    let next = (at + 1) % samples.len();
    let share = hour / step - hour.div_euclid(step) * 0.0 - (hour / step).floor();
    println!(
        "sky {id:03} at {hour:.3}h: {} samples, between {at} and {next} at {share:.4}",
        samples.len()
    );
    let of = |held: amb::Harmonics| [held.red(), held.green(), held.blue()];
    let (first, second) = (of(samples[at]), of(samples[next]));
    for channel in 0..3 {
        let held: Vec<f32> = (0..9)
            .map(|term| {
                first[channel][term] + (second[channel][term] - first[channel][term]) * share
            })
            .collect();
        // The row the shader dots against `(normal, 1)`: the three linear terms, then the constant.
        println!(
            "   row {channel}  {:>13.7} {:>13.7} {:>13.7} {:>13.7}",
            held[3] * LINEAR,
            held[1] * LINEAR,
            held[2] * LINEAR,
            held[0] * CONSTANT,
        );
    }
}
