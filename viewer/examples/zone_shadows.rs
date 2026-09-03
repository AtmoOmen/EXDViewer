//! Which of the materials a zone places answer a shadow subview, and which cast nothing.
//!
//! `zone_shadows bg/ex3/01_nvt_n4/twn/n4t1/level/n4t1.lvb`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::layer::InstanceData;
use ironworks::file::mdl::{Lod, ModelContainer};
use ironworks::file::mtrl::{self, Material};
use ironworks::file::shpk::{self, ShaderPackage, Stage};
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;

type Pack = Ironworks<SqPack<Install>>;

const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const PASS_G_SEMITRANSPARENCY: u32 = 0x6006_067f;
const PASS_Z_OPAQUE: u32 = 0xe412_a2d4;

const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;
const SUB_VIEW_SHADOW_0: u32 = 0x99b2_2d1c;

const KEYS: [(u32, u32); 3] = [
    (0xcbdf_d5ec, 0xd999_4ef1),
    (0xdcfc_844e, 0x59c4_e6db),
    (0x6313_fd87, 0x7a3d_9efd),
];

fn selector(keys: &[u32]) -> u32 {
    let (mut out, mut mul) = (0u32, 1u32);
    for key in keys {
        out = out.wrapping_add(key.wrapping_mul(mul));
        mul = mul.wrapping_mul(31);
    }
    out
}

fn values(keys: &[shpk::Key], material: &[mtrl::ShaderKey], set: &[(u32, u32)]) -> Vec<u32> {
    keys.iter()
        .map(|key| {
            set.iter()
                .find(|(id, _)| *id == key.id())
                .map(|(_, value)| *value)
                .or_else(|| {
                    material
                        .iter()
                        .find(|held| held.category() == key.id())
                        .map(mtrl::ShaderKey::value)
                })
                .unwrap_or_else(|| key.default_value())
        })
        .collect()
}

fn pair(
    package: &ShaderPackage,
    material: &[mtrl::ShaderKey],
    pass: u32,
    subview: u32,
) -> Option<(u32, u32)> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, material, &KEYS)))
    .collect();
    parts.push(selector(&[package.technique_subview()[0], subview]));
    let id = selector(&parts);

    let node = package
        .nodes()
        .iter()
        .find(|node| node.id() == id)
        .or_else(|| {
            let alias = package
                .aliases()
                .iter()
                .find(|alias| alias.selector() == id)?;
            package.nodes().get(alias.node() as usize)
        })?;
    let held = node.passes().iter().find(|held| held.id() == pass)?;
    if held.vertex() == shpk::NONE || held.pixel() == shpk::NONE {
        return None;
    }
    let base = |want: Stage| {
        package
            .shaders()
            .iter()
            .take_while(|shader| shader.stage() != want)
            .count() as u32
    };
    Some((
        base(Stage::Vertex) + held.vertex(),
        base(Stage::Pixel) + held.pixel(),
    ))
}

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
    let mut packages: BTreeMap<String, Option<ShaderPackage>> = BTreeMap::new();

    for zone in std::env::args().skip(1) {
        let level: LevelFile = ironworks.file(&zone).expect("a level");
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

        let mut materials: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for path in &models {
            let Ok(container) = ironworks.file::<ModelContainer>(path) else {
                continue;
            };
            for mesh in container.model(Lod::High).meshes() {
                if let Ok(name) = mesh.material() {
                    materials.entry(name).or_default().insert(path.clone());
                }
            }
        }

        let mut tally: BTreeMap<(String, bool, bool, bool), usize> = BTreeMap::new();
        let mut unread = 0;
        for (path, named) in &materials {
            let Ok(material) = ironworks.file::<Material>(path) else {
                unread += 1;
                continue;
            };
            let name = format!("shader/sm5/shpk/{}", material.shader());
            let package = packages.entry(name).or_insert_with_key(|name| {
                let bytes = ironworks.file::<Vec<u8>>(name).ok()?;
                ShaderPackage::parse(&bytes).ok()
            });
            let Some(package) = package else {
                println!("{path}: no package");
                continue;
            };
            let keys = material.shader_keys();
            let buffer = pair(package, keys, PASS_G_OPAQUE, SUB_VIEW_MAIN).is_some()
                || pair(package, keys, PASS_G_SEMITRANSPARENCY, SUB_VIEW_MAIN).is_some();
            let depth = pair(package, keys, PASS_Z_OPAQUE, SUB_VIEW_MAIN).is_some();
            let shadow = pair(package, keys, PASS_Z_OPAQUE, SUB_VIEW_SHADOW_0).is_some();
            if !shadow {
                println!("   no shadow  {path}  {}", material.shader());
                for held in named {
                    println!("              on {held}");
                }
            }
            *tally
                .entry((material.shader().to_owned(), buffer, depth, shadow))
                .or_default() += 1;
        }
        println!(
            "== {zone}: {} models, {} materials, {unread} unread",
            models.len(),
            materials.len()
        );
        for ((name, buffer, depth, shadow), count) in &tally {
            println!("   {count:>4}  {name:<28} g {buffer:<5} z {depth:<5} shadow {shadow}");
        }
    }
}
