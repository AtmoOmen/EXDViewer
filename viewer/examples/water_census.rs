//! Every mesh a zone places on a water package: where it stands, what it names, and whether the
//! textures it names are there.
//!
//! `water_census <path.lvb>`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::layer::InstanceData;
use ironworks::file::mdl::{Lod, MeshKind, ModelContainer, VertexAttributeKind, VertexValues};
use ironworks::file::mtrl::Material;
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;
const WET: [&str; 2] = ["water.shpk", "river.shpk"];

type Pack = Ironworks<SqPack<Install>>;

fn named(id: u32) -> String {
    shaders::names::resolve(id).map_or_else(|| format!("{id:08x}"), ToOwned::to_owned)
}

fn walk(
    ironworks: &Pack,
    instances: &[ironworks::file::layer::Instance],
    models: &mut BTreeMap<String, Vec<[f32; 3]>>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) {
    for instance in instances {
        match instance.data() {
            InstanceData::BgPart(held) if !held.asset_path().is_empty() => {
                let at = instance.transform().translation();
                models
                    .entry(held.asset_path().clone())
                    .or_default()
                    .push([at[0], at[1], at[2]]);
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

    let mut models = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for path in level.scene().layer_group_paths() {
        let Ok(file) = ironworks.file::<LayerGroupFile>(path) else {
            continue;
        };
        for layer in file.group().layers() {
            walk(&ironworks, layer.instances(), &mut models, &mut seen, 0);
        }
    }
    println!("{} models placed", models.len());

    let mut packages: BTreeMap<String, usize> = BTreeMap::new();
    let mut wet = Vec::new();
    for (path, at) in &models {
        let Ok(container) = ironworks.file::<ModelContainer>(path) else {
            continue;
        };
        let model = container.model(Lod::High);
        for (index, mesh) in model.meshes().iter().enumerate() {
            let Ok(name) = mesh.material() else {
                continue;
            };
            let Ok(material) = ironworks.file::<Material>(name.trim_start_matches('/')) else {
                continue;
            };
            *packages.entry(material.shader().to_owned()).or_default() += 1;
            if !WET.contains(&material.shader()) {
                continue;
            }
            let mut low = [f32::MAX; 3];
            let mut high = [f32::MIN; 3];
            let mut vertices = 0;
            if let Ok(attributes) = mesh.attributes() {
                for attribute in attributes {
                    if attribute.kind as u8 != VertexAttributeKind::Position as u8 {
                        continue;
                    }
                    if let VertexValues::Vector4(values) = &attribute.values {
                        vertices = values.len();
                        for value in values {
                            for lane in 0..3 {
                                low[lane] = low[lane].min(value[lane]);
                                high[lane] = high[lane].max(value[lane]);
                            }
                        }
                    }
                }
            }
            wet.push((
                path.clone(),
                index,
                name,
                material,
                mesh.kinds().to_vec(),
                mesh.indices().map(|held| held.len()).unwrap_or(0),
                vertices,
                low,
                high,
                at.clone(),
            ));
        }
    }

    println!("\npackages over every placed mesh:");
    for (name, count) in &packages {
        println!("   {count:>5}  {name}");
    }

    println!("\n{} water meshes:", wet.len());
    for (path, index, name, material, kinds, indices, vertices, low, high, at) in &wet {
        let kinds: Vec<String> = kinds.iter().map(|held| format!("{held:?}")).collect();
        println!("\n== {path} mesh {index} [{}]", kinds.join(","));
        println!("   {indices} indices, {vertices} vertices");
        println!(
            "   local x {:.2}..{:.2} y {:.2}..{:.2} z {:.2}..{:.2}",
            low[0], high[0], low[1], high[1], low[2], high[2]
        );
        for one in at.iter().take(8) {
            println!(
                "   placed at {:.2},{:.2},{:.2}",
                one[0], one[1], one[2]
            );
        }
        println!("   {name} -> {}", material.shader());
        for key in material.shader_keys() {
            println!("      key {} = {}", named(key.category()), named(key.value()));
        }
        for constant in material.constants() {
            println!(
                "      constant {} = {:?}",
                named(constant.id()),
                material.constant_values(constant),
            );
        }
        for sampler in material.samplers() {
            let texture = sampler
                .texture_index()
                .and_then(|held| material.textures().get(usize::from(held)))
                .map(|held| held.path().to_owned());
            let arrived = texture
                .as_ref()
                .map(|held| ironworks.file::<ironworks::file::tex::Texture>(held).is_ok());
            println!(
                "      sampler {} -> {} {}",
                named(sampler.id()),
                texture.as_deref().unwrap_or("-"),
                match arrived {
                    Some(true) => "ok",
                    Some(false) => "MISSING",
                    None => "",
                }
            );
        }
        if let Some(table) = material.color_table() {
            println!("      color table {:?} {} rows", table.kind(), table.rows());
        }
    }
    let standard = wet
        .iter()
        .filter(|held| held.4.contains(&MeshKind::Standard))
        .count();
    println!("\n{standard} of {} carry MeshKind::Standard", wet.len());
}
