//! Dump the raw bytecode blob of every shader in the named files, so a capture's DXVK shader
//! names (a sha1 of the bytes handed to CreateXShader) can be matched back to a game file.

use std::fs;
use std::path::Path;

use ironworks::Ironworks;
use ironworks::file::{shcd, shpk};
use ironworks::sqpack::{Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const OUT: &str = "/tmp/shaderblobs";

fn container(bytes: &[u8]) -> Option<&[u8]> {
    let at = bytes.windows(4).position(|four| four == b"DXBC")?;
    let rest = &bytes[at..];
    let size = u32::from_le_bytes(rest.get(24..28)?.try_into().ok()?) as usize;
    rest.get(..size)
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    fs::create_dir_all(OUT).unwrap();
    for path in std::env::args().skip(1) {
        let bytes: Vec<u8> = match ironworks.file(&path) {
            Ok(held) => held,
            Err(why) => {
                println!("MISS {path}: {why}");
                continue;
            }
        };
        let stem = path.replace('/', "_");
        if path.ends_with(".shcd") {
            let code = match shcd::ShaderCode::parse(&bytes) {
                Ok(held) => held,
                Err(why) => {
                    println!("BAD  {path}: {why}");
                    continue;
                }
            };
            let blob = &bytes[code.blob_offset()..code.blob_offset() + code.blob_size()];
            match container(blob) {
                Some(held) => {
                    let out = Path::new(OUT).join(format!("{stem}.0.{:?}.dxbc", code.stage()));
                    fs::write(&out, held).unwrap();
                    println!("OK   {path} {:?} {} bytes", code.stage(), held.len());
                }
                None => println!("NOMAGIC {path}"),
            }
            continue;
        }
        let package = match shpk::ShaderPackage::parse(&bytes) {
            Ok(held) => held,
            Err(why) => {
                println!("BAD  {path}: {why}");
                continue;
            }
        };
        let base = package.blobs_offset();
        for (at, shader) in package.shaders().iter().enumerate() {
            let start = base + shader.blob_offset() as usize;
            let end = start + shader.blob_size() as usize;
            let Some(blob) = bytes.get(start..end) else {
                println!("SHORT {path} #{at}");
                continue;
            };
            match container(blob) {
                Some(held) => {
                    let out = Path::new(OUT).join(format!("{stem}.{at}.{:?}.dxbc", shader.stage()));
                    fs::write(&out, held).unwrap();
                }
                None => println!("NOMAGIC {path} #{at}"),
            }
        }
        println!("OK   {path} {} shaders", package.shaders().len());
    }
}
