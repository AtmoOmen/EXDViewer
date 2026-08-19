//! Whether a model carries a skeleton, and what its meshes are named against.
//!
//! `mdl_bones <path.mdl> ...`

use ironworks::file::mdl::{Lod, ModelContainer};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let container: ModelContainer = match ironworks.file(&path) {
            Ok(held) => held,
            Err(why) => {
                println!("{path}: {why}");
                continue;
            }
        };
        let model = container.model(Lod::High);
        let mut low = [f32::MAX; 3];
        let mut high = [f32::MIN; 3];
        for mesh in model.meshes() {
            let Ok(attributes) = mesh.attributes() else { continue };
            for attribute in attributes {
                if attribute.kind as u8 != ironworks::file::mdl::VertexAttributeKind::Position as u8 {
                    continue;
                }
                let values: Vec<[f32; 3]> = match &attribute.values {
                    ironworks::file::mdl::VertexValues::Vector3(held) => held.clone(),
                    ironworks::file::mdl::VertexValues::Vector4(held) => {
                        held.iter().map(|one| [one[0], one[1], one[2]]).collect()
                    }
                    _ => continue,
                };
                for value in values {
                    for axis in 0..3 {
                        low[axis] = low[axis].min(value[axis]);
                        high[axis] = high[axis].max(value[axis]);
                    }
                }
            }
        }
        println!(
            "   bounds x {:7.2}..{:7.2}  y {:7.2}..{:7.2}  z {:7.2}..{:7.2}",
            low[0], high[0], low[1], high[1], low[2], high[2]
        );
        let bones = model.bone_names().unwrap_or_default();
        let attributes = model.attribute_names().unwrap_or_default();
        println!(
            "{path}\n   {} meshes, {} bones {:?}, {} shapes, attributes {:?}",
            model.meshes().len(),
            bones.len(),
            bones,
            model.shapes().len(),
            attributes,
        );
    }
}
