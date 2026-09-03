//! Every shader blob the install holds, reached by index hash rather than by path, so the packages
//! that ship under a hash for a name are swept too. Written out for a `sha1sum`, which is the name
//! DXVK stamps into the module a capture holds.
//!
//! `shader_sweep [out dir]`

use std::fs;
use std::io::{Read, Seek};
use std::path::Path;

use ironworks::file::{shcd, shpk};
use ironworks::sqpack::{IndexHash, Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const OUT: &str = "/tmp/shaderblobs";

/// Where the shader files sit.
const SHADER: u8 = 5;

fn container(bytes: &[u8]) -> Option<&[u8]> {
    let at = bytes.windows(4).position(|four| four == b"DXBC")?;
    let rest = &bytes[at..];
    let size = u32::from_le_bytes(rest.get(24..28)?.try_into().ok()?) as usize;
    rest.get(..size)
}

fn read<R: Read + Seek>(mut file: ironworks::sqpack::File<R>) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| OUT.to_owned());
    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    fs::create_dir_all(&out).unwrap();

    let entries = sqpack.entries().expect("the install's index");
    let shaders: Vec<_> = entries
        .iter()
        .filter(|entry| entry.category == SHADER)
        .collect();
    println!(
        "{} shader entries of {} whole",
        shaders.len(),
        entries.len()
    );

    let (mut files, mut blobs) = (0, 0);
    for entry in shaders {
        let name = match entry.hash {
            IndexHash::Split(hash) => format!("{:08x}_{:08x}", hash >> 32, hash as u32),
            IndexHash::Whole(hash) => format!("{hash:08x}"),
        };
        let Some(bytes) = sqpack
            .file_by_hash(entry.repository, entry.category, entry.hash)
            .ok()
            .and_then(read)
        else {
            println!("MISS {name}");
            continue;
        };
        files += 1;
        // A package lays its shaders out behind one header; a code file holds exactly one.
        let held: Vec<(usize, Vec<u8>)> = match shpk::ShaderPackage::parse(&bytes) {
            Ok(package) => {
                let base = package.blobs_offset();
                package
                    .shaders()
                    .iter()
                    .enumerate()
                    .filter_map(|(at, shader)| {
                        let start = base + shader.blob_offset() as usize;
                        let blob = bytes.get(start..start + shader.blob_size() as usize)?;
                        Some((at, container(blob)?.to_vec()))
                    })
                    .collect()
            }
            Err(_) => match shcd::ShaderCode::parse(&bytes) {
                Ok(code) => {
                    let blob = &bytes[code.blob_offset()..code.blob_offset() + code.blob_size()];
                    container(blob)
                        .map(|held| (0, held.to_vec()))
                        .into_iter()
                        .collect()
                }
                Err(_) => Vec::new(),
            },
        };
        for (at, blob) in held {
            fs::write(Path::new(&out).join(format!("{name}.{at}.dxbc")), &blob).unwrap();
            blobs += 1;
        }
    }
    println!("{files} files read, {blobs} blobs written to {out}");
}
