//! What the lighting packages declare, so a shadow pass can be told from the files rather than
//! guessed at. Reads the install directly.

use ironworks::{
    Ironworks,
    file::shpk::ShaderPackage,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const PACKAGES: [&str; 8] = [
    "directionallighting",
    "directionalshadow",
    "shadowmask",
    "pointlighting",
    "spotlighting",
    "bg_composite",
    "character",
    "bg",
];

fn named(id: u32) -> String {
    shaders::names::resolve(id).map_or_else(|| format!("{id:08x}"), ToOwned::to_owned)
}

fn main() {
    let sqpack = std::env::args().nth(1).unwrap_or_else(|| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    let asked: Vec<String> = std::env::args().skip(2).collect();
    let packages = match asked.is_empty() {
        true => PACKAGES.iter().map(|name| (*name).to_owned()).collect(),
        false => asked,
    };
    for name in packages {
        let path = format!("shader/sm5/shpk/{name}.shpk");
        let package: ShaderPackage = match ironworks.file(&path) {
            Ok(package) => package,
            Err(error) => {
                println!("== {name}: {error}\n");
                continue;
            }
        };
        let [technique, subview] = package.technique_subview();
        println!(
            "== {name}  {} shaders, {} nodes, technique {technique:08x} subview {subview:08x}",
            package.shaders().len(),
            package.nodes().len(),
        );
        for (band, list) in [
            ("constant", package.constants()),
            ("sampler", package.samplers()),
            ("texture", package.textures()),
            ("uav", package.uavs()),
        ] {
            for resource in list {
                println!(
                    "   {band:<9} slot {:>2}  {}",
                    resource.slot(),
                    named(resource.id())
                );
            }
        }
        for (group, keys) in [
            ("system", package.system_keys()),
            ("scene", package.scene_keys()),
        ] {
            for key in keys {
                println!("   key {group:<7} {}", named(key.id()));
            }
        }
        println!();
    }
}
