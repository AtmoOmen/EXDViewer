//! Lossless alternatives to a `.tex` file's raw bytes: an exact DDS container built from the
//! file's own blocks with no re-encode, and PNGs for the formats a browser can already open.

use std::io::{Cursor, Write as _};

use anyhow::{Context, Result, ensure};
use image::{DynamicImage, ImageFormat as PngFormat};
use image_dds::ddsfile::{
    AlphaMode, Caps2, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams,
};
use ironworks::file::tex::{self, TextureKind};
use zip::{ZipWriter, write::SimpleFileOptions};

use super::tex_loader;

/// A PNG export, zipped only when it came out to more than one file.
pub enum PackagedImages {
    Single(Vec<u8>),
    Zip(Vec<u8>),
}

impl PackagedImages {
    pub fn file_name(&self, stem: &str) -> String {
        match self {
            Self::Single(_) => format!("{stem}.png"),
            Self::Zip(_) => format!("{stem}.zip"),
        }
    }

    pub fn bytes(self) -> Vec<u8> {
        match self {
            Self::Single(bytes) | Self::Zip(bytes) => bytes,
        }
    }
}

/// The DXGI format a `.tex` pixel format maps to bit-for-bit, for a pass-through DDS. `None` for
/// anything this viewer cannot otherwise decode either, where raw `.tex` bytes stay the only
/// export -- matched one for one against `tex_loader::decode_stack`'s formats, since a mapping
/// nothing in the corpus ever uses would be untestable guesswork.
///
/// `L8Unorm` has no DXGI luminance format at all; its bytes are identical to `R8_UNorm`; a
/// third-party viewer will show it red-tinted rather than gray, which the always-available raw
/// `.tex` export settles for anyone who needs the original mapping.
fn dxgi_format(format: tex::Format) -> Option<DxgiFormat> {
    use tex::Format;
    Some(match format {
        Format::L8Unorm | Format::R8Unorm => DxgiFormat::R8_UNorm,
        Format::A8Unorm => DxgiFormat::A8_UNorm,
        Format::R8Uint => DxgiFormat::R8_UInt,
        Format::Rg8Unorm => DxgiFormat::R8G8_UNorm,
        Format::Bgra4Unorm => DxgiFormat::B4G4R4A4_UNorm,
        Format::Bgr5a1Unorm => DxgiFormat::B5G5R5A1_UNorm,
        Format::Bgra8Unorm => DxgiFormat::B8G8R8A8_UNorm,
        Format::Bgrx8Unorm => DxgiFormat::B8G8R8X8_UNorm,
        Format::R16Unorm => DxgiFormat::R16_UNorm,
        Format::Rg16Unorm => DxgiFormat::R16G16_UNorm,
        Format::R16Float => DxgiFormat::R16_Float,
        Format::Rg16Float => DxgiFormat::R16G16_Float,
        Format::Rgba16Float => DxgiFormat::R16G16B16A16_Float,
        Format::R32Float => DxgiFormat::R32_Float,
        Format::Rg32Float => DxgiFormat::R32G32_Float,
        Format::Rgba32Float => DxgiFormat::R32G32B32A32_Float,
        Format::Bc1Unorm => DxgiFormat::BC1_UNorm,
        Format::Bc2Unorm => DxgiFormat::BC2_UNorm,
        Format::Bc3Unorm => DxgiFormat::BC3_UNorm,
        Format::Bc4Unorm => DxgiFormat::BC4_UNorm,
        Format::Bc5Unorm => DxgiFormat::BC5_UNorm,
        // Matches the Sfloat interpretation `tex_loader` already decodes BC6H with.
        Format::Bc6hFloat => DxgiFormat::BC6H_SF16,
        Format::Bc7Unorm => DxgiFormat::BC7_UNorm,
        _ => return None,
    })
}

/// Pixel data in the order a DDS body expects it. A cube or array stores each face or element's
/// whole mip chain contiguously (`Surface`'s own doc: "Layer 0 Mip 0, Layer 0 Mip 1, ..., Layer
/// L-1 Mip M-1"), the reverse of the game's own mip-major storage, so those two kinds are
/// reshuffled a block at a time; a volume's mips already hold that level's whole depth before the
/// next level, which is the same layout a DDS 3D texture wants, so it passes through untouched.
fn dds_body(texture: &tex::Texture) -> Result<Vec<u8>> {
    let levels = texture.mip_levels();
    let mip = |level: u8| {
        texture
            .mip_data(level)
            .with_context(|| format!("missing mipmap level {level}"))
    };

    if !matches!(texture.kind(), TextureKind::Cube | TextureKind::D2Array) {
        let mut body = Vec::with_capacity(texture.data().len());
        for level in 0..levels {
            body.extend_from_slice(mip(level)?);
        }
        return Ok(body);
    }

    let total_layers = texture.layers(0);
    let mut body = Vec::with_capacity(texture.data().len());
    for layer in 0..total_layers {
        for level in 0..levels {
            let data = mip(level)?;
            let layers_at_level = usize::from(texture.layers(level).max(1));
            let stride = data.len() / layers_at_level;
            ensure!(
                stride > 0,
                "mipmap level {level} holds no {layers_at_level} layers"
            );
            let start = usize::from(layer)
                .checked_mul(stride)
                .context("layer offset overflows")?;
            let end = start.checked_add(stride).context("layer span overflows")?;
            body.extend_from_slice(
                data.get(start..end)
                    .context("layer slice runs past its mipmap level")?,
            );
        }
    }
    Ok(body)
}

/// The whole texture -- every mip level, every face, layer or depth slice -- as a DDS file, built
/// by relaying the game's own compressed or raw bytes into the container's header rather than
/// decoding and re-encoding them.
pub fn dds(texture: &tex::Texture) -> Result<Vec<u8>> {
    let format = dxgi_format(texture.format())
        .with_context(|| format!("no DDS mapping for {:?}", texture.format()))?;
    let kind = texture.kind();
    let mut file = Dds::new_dxgi(NewDxgiParams {
        height: u32::from(texture.height()),
        width: u32::from(texture.width()),
        depth: (kind == TextureKind::D3).then(|| u32::from(texture.depth())),
        format,
        mipmap_levels: (texture.mip_levels() > 1).then(|| u32::from(texture.mip_levels())),
        // `Dds::new_dxgi` sizes its data buffer as `array_layers * one layer's mip chain`, then
        // divides by 6 for the header10 field only when `is_cubemap`; passing `None` here for a
        // cube under-sizes the buffer by 6x rather than being implicitly "times six" already.
        array_layers: matches!(kind, TextureKind::D2Array | TextureKind::Cube)
            .then(|| u32::from(texture.layers(0))),
        caps2: (kind == TextureKind::Cube).then_some(Caps2::CUBEMAP | Caps2::CUBEMAP_ALLFACES),
        is_cubemap: kind == TextureKind::Cube,
        resource_dimension: if kind == TextureKind::D3 {
            D3D10ResourceDimension::Texture3D
        } else {
            D3D10ResourceDimension::Texture2D
        },
        alpha_mode: AlphaMode::Straight,
    })
    .map_err(|error| anyhow::anyhow!("dds header: {error}"))?;

    let body = dds_body(texture)?;
    ensure!(
        body.len() == file.data.len(),
        "assembled {} bytes of pixel data, dds header expects {}",
        body.len(),
        file.data.len()
    );
    file.data = body;

    let mut bytes = Cursor::new(Vec::new());
    file.write(&mut bytes).context("writing dds")?;
    Ok(bytes.into_inner())
}

fn is_plain_8bit(format: tex::Format) -> bool {
    use tex::Format;
    matches!(
        format,
        Format::A8Unorm
            | Format::L8Unorm
            | Format::R8Unorm
            | Format::R8Uint
            | Format::Rg8Unorm
            | Format::Bgrx8Unorm
            | Format::Bgra8Unorm
            | Format::Bgra4Unorm
            | Format::Bgr5a1Unorm
    )
}

fn is_plain_16bit(format: tex::Format) -> bool {
    matches!(format, tex::Format::R16Unorm | tex::Format::Rg16Unorm)
}

/// PNG export for the formats a PNG can hold exactly: 8-bit for the plain integer formats, 16-bit
/// for `R16Unorm`/`Rg16Unorm`. `None` for block-compressed and float formats, where `dds` is the
/// lossless option instead. A cube, array or volume comes back as one PNG per face, layer or
/// slice, packaged into a zip; a plain 2D texture is a single PNG.
pub fn png(texture: &tex::Texture, level: u8, path: &str) -> Result<Option<PackagedImages>> {
    let format = texture.format();
    let layers = texture.layers(level);

    if is_plain_16bit(format) {
        if layers > 1 {
            // Never observed in the corpus: every 16-bit-unorm texture on record is a plain 2D
            // one. DDS still exports this losslessly; a 16-bit zip is not worth building untested.
            return Ok(None);
        }
        let (width, height) = texture.mip_size(level);
        let data = texture
            .mip_data(level)
            .with_context(|| format!("texture has no mipmap level {level}"))?;
        let image =
            tex_loader::read_unorm16_precise(width, height, data, usize::from(format.components()))?;
        return Ok(Some(PackagedImages::Single(tex_loader::write(
            image,
            PngFormat::Png,
        )?)));
    }

    if !is_plain_8bit(format) {
        return Ok(None);
    }

    let stack = tex_loader::decode_stack(texture, level, path)?;
    let (_, slice_height) = texture.mip_size(level);
    Ok(Some(slice_png(&stack, slice_height, layers, texture.kind())?))
}

fn slice_png(
    stack: &DynamicImage,
    slice_height: u16,
    layers: u16,
    kind: TextureKind,
) -> Result<PackagedImages> {
    if layers <= 1 {
        return Ok(PackagedImages::Single(tex_loader::write(
            stack.clone(),
            PngFormat::Png,
        )?));
    }

    let label = match kind {
        TextureKind::Cube => "face",
        TextureKind::D3 => "slice",
        _ => "layer",
    };
    let width = stack.width();
    let slice_height = u32::from(slice_height);
    let mut files = Vec::with_capacity(usize::from(layers));
    for index in 0..layers {
        let cropped = stack.crop_imm(0, u32::from(index) * slice_height, width, slice_height);
        let bytes = tex_loader::write(cropped, PngFormat::Png)?;
        files.push((format!("{label}{index}.png"), bytes));
    }
    Ok(PackagedImages::Zip(zip_files(&files)?))
}

fn zip_files(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in files {
        archive.start_file(name, SimpleFileOptions::default())?;
        archive.write_all(bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironworks::file::File as _;
    use std::io::Cursor as ReadCursor;

    /// A one-mip, one-level texture header plus a body, so the header math (offsets, `mip_data`)
    /// is exercised the same way a real file would be. `attributes`, `format`, `width`, `height`,
    /// `depth`, `mip_levels`, `array_size` occupy the first 16 bytes; `lod_surfaces` the next 12;
    /// `surface_offsets` the 52 after that, ending exactly at `Texture::HEADER_SIZE`.
    fn texture(
        kind: u32,
        format: u32,
        width: u16,
        height: u16,
        depth: u16,
        array_size: u8,
        data: &[u8],
    ) -> tex::Texture {
        let mut bytes = vec![0u8; 80];
        bytes[..4].copy_from_slice(&(kind << 22).to_le_bytes());
        bytes[4..8].copy_from_slice(&format.to_le_bytes());
        bytes[8..10].copy_from_slice(&width.to_le_bytes());
        bytes[10..12].copy_from_slice(&height.to_le_bytes());
        bytes[12..14].copy_from_slice(&depth.to_le_bytes());
        bytes[14] = 1; // mip_levels
        bytes[15] = array_size;
        bytes[28..32].copy_from_slice(&80u32.to_le_bytes()); // surface_offsets[0]
        bytes.extend_from_slice(data);
        tex::Texture::read(ReadCursor::new(bytes)).unwrap()
    }

    #[test]
    fn a_plain_2d_texture_dds_round_trips_through_image_dds() {
        // Bgra8Unorm, 2x2, four distinct pixels so a channel swap or wrong stride would show.
        let pixels: Vec<u8> = (0..16).collect();
        let tex = texture(0b0000010, 0x1450, 2, 2, 1, 0, &pixels);

        let dds_bytes = dds(&tex).unwrap();
        let file = Dds::read(&mut ReadCursor::new(dds_bytes)).unwrap();
        assert_eq!(file.get_dxgi_format(), Some(DxgiFormat::B8G8R8A8_UNorm));
        assert_eq!(file.data, pixels);

        let decoded = image_dds::image_from_dds(&file, 0).unwrap();
        let expected = tex_loader::decode_stack(&tex, 0, "test").unwrap();
        assert_eq!(decoded.as_raw(), expected.to_rgba8().as_raw());
    }

    #[test]
    fn a_volume_keeps_its_mip_major_order_unchanged() {
        // Depth 2 at mip 0: two 2x2 slices, one byte per pixel.
        let data = vec![1u8; 2 * 2 * 2];
        let tex = texture(0b0000100, 0x1132, 2, 2, 2, 0, &data);
        assert_eq!(dds_body(&tex).unwrap(), tex.mip_data(0).unwrap());
    }

    #[test]
    fn a_cube_reorders_from_mip_major_to_layer_major() {
        // Two mip levels, six faces. Mip 0 is 2x2 (4 bytes/face), mip 1 is 1x1 (1 byte/face); the
        // game stores mip 0's six faces, then mip 1's six faces. A face's own two bytes should
        // read as [mip0 face, mip1 face] once reordered by layer.
        let mip0: Vec<u8> = (0..6).flat_map(|face| vec![face; 4]).collect();
        let mip1: Vec<u8> = (0u8..6).map(|face| 100 + face).collect();
        let mut bytes = vec![0u8; 80];
        bytes[..4].copy_from_slice(&(0b0001000u32 << 22).to_le_bytes());
        bytes[4..8].copy_from_slice(&0x1132u32.to_le_bytes()); // R8Unorm
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
        bytes[14] = 2; // mip_levels
        bytes[28..32].copy_from_slice(&80u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&(80 + mip0.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&mip0);
        bytes.extend_from_slice(&mip1);
        let tex = tex::Texture::read(ReadCursor::new(bytes)).unwrap();

        let body = dds_body(&tex).unwrap();
        for face in 0..6usize {
            let chunk = &body[face * 5..face * 5 + 5];
            assert_eq!(chunk, [face as u8, face as u8, face as u8, face as u8, 100 + face as u8]);
        }
    }

    #[test]
    fn r16_unorm_png_keeps_all_16_bits() {
        // 0x1234 would collapse to a single byte under the 8-bit preview path.
        let data = 0x1234u16.to_le_bytes().to_vec();
        let tex = texture(0b0000010, 0x7140, 1, 1, 1, 0, &data);
        let PackagedImages::Single(png_bytes) = png(&tex, 0, "test").unwrap().unwrap() else {
            panic!("a single 2D texture should not zip");
        };
        assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
        let bit_depth = png_bytes[24];
        assert_eq!(bit_depth, 16, "IHDR should claim 16-bit depth");

        let decoded = image::load_from_memory(&png_bytes).unwrap();
        assert_eq!(decoded.to_luma16().get_pixel(0, 0).0, [0x1234]);
    }

    #[test]
    fn a_bc_format_has_no_png_but_has_a_dds_mapping() {
        let tex = texture(0b0000010, 0x3420, 4, 4, 1, 0, &vec![0u8; 8]);
        assert!(png(&tex, 0, "test").unwrap().is_none());
        assert!(dxgi_format(tex.format()).is_some());
    }

    const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

    fn read_local(path: &str) -> Vec<u8> {
        use ironworks::sqpack::{Install, SqPack};
        use std::io::Read;
        let pack = SqPack::new(Install::at_sqpack(SQPACK));
        let mut stream = pack.file(path).unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    }

    /// A real 3D LUT (16x16x16 `Bgrx8Unorm`, one mip). `image_dds`'s own RGBA8 surface decoder
    /// does not implement `B8G8R8X8_UNorm`, so this checks the header and the passthrough body
    /// directly rather than round-tripping through it; the body is the exact bytes
    /// `tex_loader::decode_stack` already decodes on screen for this same file.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_real_volume_lut_dds_is_a_header_over_the_untouched_body() {
        let bytes = read_local("common/graphics/texture/-output_lut_p.tex");
        let tex = tex::Texture::read(ReadCursor::new(bytes)).unwrap();
        assert_eq!(tex.kind(), TextureKind::D3);

        let dds_bytes = dds(&tex).unwrap();
        let file = Dds::read(&mut ReadCursor::new(dds_bytes)).unwrap();
        assert_eq!(file.get_dxgi_format(), Some(DxgiFormat::B8G8R8X8_UNorm));
        assert_eq!(file.get_width(), u32::from(tex.width()));
        assert_eq!(file.get_height(), u32::from(tex.height()));
        assert_eq!(file.get_depth(), u32::from(tex.depth()));
        assert_eq!(file.data, tex.mip_data(0).unwrap());
    }

    /// A real BC1 cube (six faces, eight mip levels), covering the same reorder code path as the
    /// `D2Array` test below plus the cubemap header flags (`is_cubemap`, `Caps2::CUBEMAP`).
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_real_bc1_cube_survives_the_reorder_at_every_mip() {
        let path = "bgcommon/nature/envmap/texture/_n_envmap_000.tex";
        let bytes = read_local(path);
        let tex = tex::Texture::read(ReadCursor::new(bytes)).unwrap();
        assert_eq!(tex.kind(), TextureKind::Cube);

        let dds_bytes = dds(&tex).unwrap();
        let file = Dds::read(&mut ReadCursor::new(dds_bytes)).unwrap();
        assert_eq!(file.get_dxgi_format(), Some(DxgiFormat::BC1_UNorm));
        for level in 0..tex.mip_levels() {
            let decoded = image_dds::image_from_dds(&file, u32::from(level)).unwrap();
            let expected = tex_loader::decode_stack(&tex, level, path).unwrap();
            assert_eq!(
                decoded.as_raw(),
                expected.to_rgba8().as_raw(),
                "mip {level}"
            );
        }
    }

    /// A real BC7 `D2Array` (the reorder code path also used for `Cube`), run manually (`cargo
    /// test -p viewer --lib -- --ignored tex_export::tests::a_real --nocapture`): decodes every
    /// mip level both through `image_dds` off the assembled DDS and through `decode_stack` off
    /// the original file, and requires the two to agree pixel for pixel. Mip 0 catches a wrong
    /// layer/mip order; the deepest mip catches a stride or block-padding mistake, since by then a
    /// slice has shrunk below BC7's 4x4 block grid.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_real_bc7_array_survives_the_reorder_at_every_mip() {
        let path = "chara/common/texture/tile_norm_array.tex";
        let bytes = read_local(path);
        let tex = tex::Texture::read(ReadCursor::new(bytes)).unwrap();
        assert_eq!(tex.kind(), TextureKind::D2Array);

        let dds_bytes = dds(&tex).unwrap();
        let file = Dds::read(&mut ReadCursor::new(dds_bytes)).unwrap();
        for level in 0..tex.mip_levels() {
            let decoded = image_dds::image_from_dds(&file, u32::from(level)).unwrap();
            let expected = tex_loader::decode_stack(&tex, level, path).unwrap();
            assert_eq!(
                decoded.as_raw(),
                expected.to_rgba8().as_raw(),
                "mip {level}"
            );
        }
    }
}
