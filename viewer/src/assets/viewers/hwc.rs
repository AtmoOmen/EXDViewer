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
        ("格式", "每通道 8 位，alpha 在前".to_owned()),
        ("尺寸", format!("{} x {}", hwc::WIDTH, hwc::HEIGHT)),
        ("文件大小", Bytes(bytes.len()).to_string()),
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
        export::Choice::bytes("作为 BMP", "cursor.bmp", move || {
            crate::utils::tex_loader::write(pixels(bytes)?, image::ImageFormat::Bmp)
        })
        .filter("BMP 图像", &["bmp"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironworks::file::hwc;

    /// No real file is needed: a `.hwc` is a bare buffer with no header, so a synthetic one of the
    /// right size is indistinguishable from a shipped one to this reader.
    #[test]
    fn the_bmp_choice_round_trips_the_pixels() {
        let mut raw = vec![0u8; hwc::WIDTH * hwc::HEIGHT * 4];
        // One pixel set to a distinctive, non-symmetric BGRA value so the round trip is checked
        // against real content rather than an all-zero buffer that would pass by accident.
        raw[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);

        let image = pixels(&raw).expect("a full-size buffer parses");
        let bmp = crate::utils::tex_loader::write(image.clone(), image::ImageFormat::Bmp)
            .expect("the image crate can write a BMP with an alpha channel");
        assert_eq!(&bmp[0..2], b"BM", "a BMP starts with its own magic");

        let read_back = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp)
            .expect("the image crate can read back what it wrote")
            .to_rgba8();
        assert_eq!(read_back.dimensions(), image.dimensions());
        assert_eq!(read_back.as_raw(), image.as_raw());
    }
}
