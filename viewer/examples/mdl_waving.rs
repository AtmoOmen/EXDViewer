//! Whether a model may wave, and what the stream the waving shader reads actually holds.
//!
//! `mdl_waving <path.mdl> ...`

use ironworks::file::mdl::{Lod, ModelContainer, VertexAttributeKind, VertexValues};
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
        let meshes = model.meshes();
        let mut low = [f32::MAX; 4];
        let mut high = [f32::MIN; 4];
        let mut streams = 0;
        for mesh in meshes {
            let Ok(attributes) = mesh.attributes() else {
                continue;
            };
            for attribute in attributes {
                if attribute.kind as u8 != VertexAttributeKind::Color as u8
                    || attribute.usage_index != 1
                {
                    continue;
                }
                streams += 1;
                let VertexValues::Vector4(values) = &attribute.values else {
                    continue;
                };
                for value in values {
                    for lane in 0..4 {
                        low[lane] = low[lane].min(value[lane]);
                        high[lane] = high[lane].max(value[lane]);
                    }
                }
            }
        }
        let spell = |held: [f32; 4]| {
            held.map(|one| match one.is_finite() {
                true => format!("{one:.2}"),
                false => "-".to_owned(),
            })
            .join(",")
        };
        println!(
            "waving {:<5} color streams {streams:<3} low {} high {}  {path}",
            model.waving(),
            spell(low),
            spell(high)
        );
    }
}
