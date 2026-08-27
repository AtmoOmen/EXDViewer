//! Where each of a package's shaders sits in the file, and which node and pass runs it, so a
//! capture's DXVK shader names can be matched back to a pass by byte offset.
//!
//! `shpk_blob_map shader/sm5/shpk/water.shpk ...`

use ironworks::file::shpk::{ShaderPackage, Stage};
use ironworks::{
    sqpack::{Install, SqPack},
    Ironworks,
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn named(id: u32) -> String {
    shaders::names::resolve(id).map_or_else(|| format!("{id:08x}"), ToOwned::to_owned)
}

fn tag(stage: Stage) -> &'static str {
    match stage {
        Stage::Vertex => "vs",
        Stage::Pixel => "ps",
        Stage::Hull => "hs",
        Stage::Domain => "ds",
        Stage::Geometry => "gs",
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let bytes: Vec<u8> = ironworks.file(&path).expect("package");
        let package = ShaderPackage::parse(&bytes).expect("package");
        let head = package.blobs_offset();
        let name = path.rsplit('/').next().unwrap_or(&path);
        println!(
            "package {name} blobs {head} shaders {}",
            package.shaders().len()
        );
        let mut base = [0u32; 5];
        for (at, stage) in [
            Stage::Vertex,
            Stage::Pixel,
            Stage::Hull,
            Stage::Domain,
            Stage::Geometry,
        ]
        .into_iter()
        .enumerate()
        {
            base[at] = package
                .shaders()
                .iter()
                .take_while(|shader| shader.stage() != stage)
                .count() as u32;
        }
        for (at, shader) in package.shaders().iter().enumerate() {
            let start = head + shader.blob_offset() as usize;
            println!(
                "shader {at} {} {start} {}",
                tag(shader.stage()),
                start + shader.blob_size() as usize
            );
            let label = |resource: &ironworks::file::shpk::Resource| {
                package
                    .name(resource)
                    .map(str::to_owned)
                    .or_else(|| shaders::names::resolve(resource.id()).map(str::to_owned))
                    .unwrap_or_else(|| format!("{:08x}", resource.id()))
            };
            for (kind, list) in [
                ("const", shader.constants()),
                ("sampler", shader.samplers()),
                ("texture", shader.textures()),
                ("uav", shader.uavs()),
            ] {
                for resource in list {
                    println!("res {at} {kind} {} {}", resource.slot(), label(resource));
                }
            }
        }
        for key in package.system_keys() {
            println!("key system {}", named(key.id()));
        }
        for key in package.scene_keys() {
            println!("key scene {}", named(key.id()));
        }
        for key in package.material_keys() {
            println!("key material {}", named(key.id()));
        }
        for node in package.nodes() {
            let keys: Vec<String> = node.keys().iter().map(|value| named(*value)).collect();
            println!("node {:08x} {}", node.id(), keys.join(","));
            for pass in node.passes() {
                let list: Vec<String> = pass
                    .stages()
                    .iter()
                    .enumerate()
                    .map(|(at, index)| match *index {
                        u32::MAX => "-".to_owned(),
                        held => (base[at] + held).to_string(),
                    })
                    .collect();
                println!(
                    "pass {:08x} {} {}",
                    node.id(),
                    named(pass.id()),
                    list.join(" ")
                );
            }
        }
    }
}
