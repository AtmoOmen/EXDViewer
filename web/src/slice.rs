use std::io::Cursor;

use bytes::Bytes;
use ironworks::file::{File, mdl::Lods, shpk::Spans, tex::Texture};
use serde::Deserialize;

/// Where a texture's mipmap offsets sit in its head.
const SURFACES: usize = 28;

/// The part of a file a request asks for. Without one the file is served whole, which is what a
/// client predating this sends and what a browser asking for a range gets.
#[derive(Debug, Default, Deserialize)]
pub struct Slice {
    /// A model's detail level.
    lod: Option<u8>,
    /// A texture's longest edge, in pixels.
    mip: Option<u16>,
    /// A shader package's tables and string block, with its bytecode left behind.
    tables: Option<u8>,
}

impl Slice {
    /// The bytes the request asked for, alongside what the response header calls them. `None`
    /// leaves the file whole, whether because nothing was asked for or because the file does not
    /// hold what was.
    pub fn cut(&self, bytes: &Bytes) -> Option<(Bytes, String)> {
        if let Some(lod) = self.lod {
            let (bytes, level) = model(bytes, lod)?;
            return Some((bytes, format!("lod={level}")));
        }
        if let Some(max_dim) = self.mip {
            return Some((texture(bytes, max_dim)?, "mip".to_owned()));
        }
        match self.tables {
            Some(1..) => Some((package(bytes)?, "tables".to_owned())),
            _ => None,
        }
    }
}

/// One detail level's geometry, with the head that names it rewritten to point straight at it.
fn model(bytes: &Bytes, lod: u8) -> Option<(Bytes, u8)> {
    let lods = Lods::read(bytes)?;
    let level = lods.level(lod);
    let (start, span) = (lods.head()?, lods.span(level)?);
    let mut held = bytes.get(..usize::try_from(start).ok()?)?.to_vec();
    lods.keep(&mut held, level);
    held.extend_from_slice(bytes.get(span.start as usize..span.end as usize)?);
    Some((held.into(), level))
}

/// The head and the one mipmap covering `max_dim`, with the head's offsets patched so that mipmap
/// lands at level zero.
fn texture(bytes: &Bytes, max_dim: u16) -> Option<Bytes> {
    let texture = Texture::read(Cursor::new(bytes.to_vec())).ok()?;
    let level = texture.level_covering(max_dim);
    let from = texture.mip_offset(level)?;
    let to = texture.mip_offset(level + 1);
    let head = usize::try_from(Texture::HEADER_SIZE).ok()?;
    let mut held = bytes.get(..head)?.to_vec();
    let at = SURFACES + usize::from(level) * 4;
    held.get_mut(at..at + 4)?
        .copy_from_slice(&Texture::HEADER_SIZE.to_le_bytes());
    if let Some(next) = held.get_mut(at + 4..at + 8) {
        next.copy_from_slice(&0u32.to_le_bytes());
    }
    let end = to.map_or(bytes.len(), |to| to as usize);
    held.extend_from_slice(bytes.get(from as usize..end)?);
    Some(held.into())
}

/// The tables and the string block with nothing between them, so the hole the bytecode leaves
/// costs no transfer. The head states where it goes.
fn package(bytes: &Bytes) -> Option<Bytes> {
    let spans = Spans::read(bytes)?;
    if bytes.len() != spans.size as usize {
        return None;
    }
    let mut held = bytes.get(..spans.blobs as usize)?.to_vec();
    held.extend_from_slice(bytes.get(spans.strings as usize..spans.size as usize)?);
    Some(held.into())
}
