//! Which packages declare a named resource, by the crc their tables key it under.
//!
//! `shpk_uses cFogParam [more names]`

use ironworks::file::shpk::ShaderPackage;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const LIST: &str = include_str!("../../smoke/shpk_names.txt");

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let wanted: Vec<(String, u32)> = std::env::args()
        .skip(1)
        .map(|name| {
            let id = shaders::names::hash(name.as_bytes());
            (name, id)
        })
        .collect();
    for (name, id) in &wanted {
        println!("== {name} ({id:08x})");
    }
    for path in LIST.split_whitespace() {
        let Ok(raw) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(package) = ShaderPackage::parse(&raw) else {
            continue;
        };
        let held: Vec<&str> = wanted
            .iter()
            .filter(|(_, id)| {
                package
                    .constants()
                    .iter()
                    .chain(package.textures())
                    .chain(package.samplers())
                    .any(|held| held.id() == *id)
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if !held.is_empty() {
            println!(
                "{:<44} {}",
                path.rsplit('/').next().unwrap_or(path),
                held.join(", ")
            );
        }
    }
}
