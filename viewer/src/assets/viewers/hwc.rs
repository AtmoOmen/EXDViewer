//! `.hwc` hardware cursors: a bare 64x64 pixel buffer in the shape the operating system takes.
//!
//! Alpha is the first byte of each pixel; the three after it are the color, and which order they
//! sit in cannot be told from the art the game ships, since it is gray.

use anyhow::Result;

use super::{Preview, upload};
use crate::assets::{Bytes, Channels};
use crate::utils::export;

fn pixels(bytes: &[u8]) -> Result<image::RgbaImage> {
    use ironworks::file::{File as _, hwc};

    let cursor = hwc::HardwareCursor::read(std::io::Cursor::new(bytes.to_vec()))?;
    let pixels = cursor
        .data()
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[3], pixel[2], pixel[1], pixel[0]])
        .collect();
    Ok(
        image::RgbaImage::from_raw(hwc::WIDTH as u32, hwc::HEIGHT as u32, pixels)
            .expect("the cursor is a whole image"),
    )
}

pub fn decode(
    ctx: &egui::Context,
    path: &str,
    bytes: &[u8],
    channels: Channels,
) -> Result<Preview> {
    use ironworks::file::hwc;

    let image = pixels(bytes)?;

    let facts = vec![
        ("Format", "8 bits per channel, alpha first".to_owned()),
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

/// Beyond the raw file: the same pixels as a `.bmp`, which every image tool opens with no decoder
/// of its own.
pub fn export_choices(bytes: &[u8]) -> Vec<export::Choice<'_>> {
    vec![
        export::Choice::bytes("As BMP", "cursor.bmp", move || {
            crate::utils::tex_loader::write(pixels(bytes)?, image::ImageFormat::Bmp)
        })
        .filter("BMP image", &["bmp"]),
    ]
}
