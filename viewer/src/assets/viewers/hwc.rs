//! `.hwc` hardware cursors: a bare 64x64 pixel buffer in the shape the operating system takes.
//!
//! Alpha is the first byte of each pixel; the three after it are the color, and which order they
//! sit in cannot be told from the art the game ships, since it is gray.

use anyhow::Result;

use super::{Preview, upload};
use crate::assets::{Bytes, Channels};

pub fn decode(
    ctx: &egui::Context,
    path: &str,
    bytes: &[u8],
    channels: Channels,
) -> Result<Preview> {
    use ironworks::file::{File as _, hwc};

    let cursor = hwc::HardwareCursor::read(std::io::Cursor::new(bytes.to_vec()))?;
    let pixels = cursor
        .data()
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[3], pixel[2], pixel[1], pixel[0]])
        .collect();
    let image = image::RgbaImage::from_raw(hwc::WIDTH as u32, hwc::HEIGHT as u32, pixels)
        .expect("the cursor is a whole image");

    let facts = vec![
        ("Format", "BGRA, alpha first".to_owned()),
        ("Dimensions", format!("{} x {}", hwc::WIDTH, hwc::HEIGHT)),
        ("File size", Bytes(bytes.len()).to_string()),
    ];

    log::info!("assets/hwc: {path}");

    Ok(upload(
        ctx,
        path,
        image.into(),
        1,
        4,
        facts,
        Vec::new(),
        channels,
    ))
}
