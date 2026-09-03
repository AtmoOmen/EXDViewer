//! Every model a zone places, whether its header lets the wind reach it, and whether it carries the
//! stream the waving shader sways it by.
//!
//! `zone_waving <path.lvb>`

use std::collections::BTreeSet;

use ironworks::file::layer::InstanceData;
use ironworks::file::mdl::{Lod, ModelContainer, VertexAttributeKind, VertexValues};
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;

type Pack = Ironworks<SqPack<Install>>;

fn walk(
    ironworks: &Pack,
    instances: &[ironworks::file::layer::Instance],
    models: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) {
    for instance in instances {
        match instance.data() {
            InstanceData::BgPart(held) if !held.asset_path().is_empty() => {
                models.insert(held.asset_path().clone());
            }
            InstanceData::SharedGroup(held)
                if depth < DEPTH
                    && !held.asset_path().is_empty()
                    && seen.insert(held.asset_path().clone()) =>
            {
                let Ok(file) = ironworks.file::<SharedGroupFile>(held.asset_path()) else {
                    continue;
                };
                for group in file.scene().layer_groups() {
                    for layer in group.layers() {
                        walk(ironworks, layer.instances(), models, seen, depth + 1);
                    }
                }
            }
            _ => (),
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let zone = std::env::args().nth(1).expect("a level path");
    let level: LevelFile = ironworks.file(&zone).unwrap();

    let mut models = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for path in level.scene().layer_group_paths() {
        let Ok(file) = ironworks.file::<LayerGroupFile>(path) else {
            continue;
        };
        for layer in file.group().layers() {
            walk(&ironworks, layer.instances(), &mut models, &mut seen, 0);
        }
    }

    let mut counts = [0usize; 4];
    let mut swaying: BTreeSet<String> = BTreeSet::new();
    let mut still: BTreeSet<String> = BTreeSet::new();
    for path in &models {
        let Ok(container) = ironworks.file::<ModelContainer>(path) else {
            continue;
        };
        let model = container.model(Lod::High);
        let waving = model.waving();
        let mut low = [f32::MAX; 4];
        let mut high = [f32::MIN; 4];
        let mut stream = false;
        for mesh in model.meshes() {
            let Ok(attributes) = mesh.attributes() else {
                continue;
            };
            for attribute in attributes {
                if attribute.kind as u8 != VertexAttributeKind::Color as u8
                    || attribute.usage_index != 1
                {
                    continue;
                }
                stream = true;
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
        let into = match waving {
            true => &mut swaying,
            false => &mut still,
        };
        for mesh in model.meshes() {
            if let Ok(name) = mesh.material() {
                into.insert(name);
            }
        }
        counts[usize::from(waving) * 2 + usize::from(stream)] += 1;
        if waving {
            let spell = |held: [f32; 4]| {
                held.map(|one| match one.is_finite() {
                    true => format!("{one:.2}"),
                    false => "-".to_owned(),
                })
                .join(",")
            };
            println!(
                "stream {stream:<5} low {} high {}  {path}",
                spell(low),
                spell(high)
            );
        }
    }
    let shared: Vec<&String> = swaying.intersection(&still).collect();
    println!("\n{} materials on a waving model, {} shared with a still one", swaying.len(), shared.len());
    for name in shared {
        println!("   {name}");
    }
    println!(
        "\n{} models: {} still, {} still with a stream, {} waving with none, {} waving with one",
        models.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3]
    );
}
