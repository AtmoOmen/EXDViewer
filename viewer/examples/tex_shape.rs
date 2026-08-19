//! The shape a texture states for itself, which is what fixes the coordinates a shader reads it at.

use ironworks::file::tex;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        match ironworks.file::<tex::Texture>(&path) {
            Ok(held) => println!(
                "{:<52} {:?}  {} x {} x {}  {} mips  {:?}",
                path.rsplit('/').next().unwrap_or(&path),
                held.kind(),
                held.width(),
                held.height(),
                held.depth(),
                held.mip_levels(),
                held.format(),
            ),
            Err(why) => println!("{path}: {why}"),
        }
    }
}
