//! Scratch tool: dump each shader's exact DXBC container bytes from a set of .shpk packages,
//! so a caller can sha1 them and match against DXVK's embedded SPIR-V debug names.
//!
//! `starfind <out_dir> <path.shpk> [path2.shpk ...]`

use ironworks::file::shcd::{self, ShaderCode};
use ironworks::file::shpk::{ShaderPackage, Stage};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn dump_container(raw: &[u8], start: usize, end: usize, out: &str) {
    let Some(blob) = raw.get(start..end) else {
        return;
    };
    let Some(container) = dxbc::scan_dxbc(blob).into_iter().next() else {
        return;
    };
    let exact = &blob[container.offset_in_file..][..container.total_size as usize];
    eprintln!(
        "{out}: blob {} B, DXBC at +{}, total_size {}",
        blob.len(),
        container.offset_in_file,
        container.total_size
    );
    std::fs::write(out, exact).unwrap();
}

fn stage_ext(stage: shcd::Stage) -> &'static str {
    match stage {
        shcd::Stage::Vertex => "vs",
        shcd::Stage::Pixel => "ps",
        shcd::Stage::Hull => "hs",
        shcd::Stage::Domain => "ds",
        shcd::Stage::Geometry => "gs",
        shcd::Stage::Compute => "cs",
        shcd::Stage::Unknown(_) => "xx",
    }
}

fn main() {
    let ironworks =
        Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK.to_owned())));
    let mut args = std::env::args().skip(1);
    let out_dir = args.next().expect("an output dir");
    std::fs::create_dir_all(&out_dir).unwrap();

    for path in args {
        let raw: Vec<u8> = match ironworks.file(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("skip {path}: {e}");
                continue;
            }
        };
        let base = path
            .rsplit('/')
            .next()
            .unwrap_or(&path)
            .replace(".shpk", "")
            .replace(".shcd", "");

        if path.ends_with(".shcd") {
            let code = match ShaderCode::parse(&raw) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("skip {path}: {e}");
                    continue;
                }
            };
            let stage = stage_ext(code.stage());
            let name = format!("{out_dir}/{base}_{stage}.dxbc");
            dump_container(&raw, code.blob_offset(), code.blob_offset() + code.blob_size(), &name);
            println!("{path}: 1 shader");
            continue;
        }

        let package = match ShaderPackage::parse(&raw) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skip {path}: {e}");
                continue;
            }
        };
        for (i, shader) in package.shaders().iter().enumerate() {
            let stage = match shader.stage() {
                Stage::Vertex => "vs",
                Stage::Pixel => "ps",
                Stage::Hull => "hs",
                Stage::Domain => "ds",
                Stage::Geometry => "gs",
            };
            let start = package.blobs_offset() + shader.blob_offset() as usize;
            let end = start + shader.blob_size() as usize;
            let name = format!("{out_dir}/{base}_{i:04}_{stage}.dxbc");
            dump_container(&raw, start, end, &name);
        }
        println!("{path}: {} shaders", package.shaders().len());
    }
}
