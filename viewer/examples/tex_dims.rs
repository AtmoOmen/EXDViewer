//! Scratch: print width/height/format of texture files named on the command line.

use ironworks::{
    Ironworks,
    file::tex::Texture,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let sqpack = std::env::var("SQPACK").unwrap_or_else(|_| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    for path in std::env::args().skip(1) {
        match ironworks.file::<Texture>(&path) {
            Ok(tex) => println!(
                "{path}: {}x{} format={:?} mips={}",
                tex.width(),
                tex.height(),
                tex.format(),
                tex.mip_levels()
            ),
            Err(e) => println!("{path}: err {e}"),
        }
    }
}
