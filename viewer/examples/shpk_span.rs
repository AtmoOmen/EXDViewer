//! What a shader package is made of, and how much of it a zone's materials actually select.
//!
//! `shpk_span bg/ex5/02_ykt_y6/twn/y6t1/`

use std::collections::{BTreeMap, BTreeSet};

use ironworks::file::mtrl::{self, Material};
use ironworks::file::shpk::{self, ShaderPackage, Stage};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const PASS_G_SEMITRANSPARENCY: u32 = 0x6006_067f;
const PASS_Z_OPAQUE: u32 = 0xe412_a2d4;
const PASS_LIGHTING_SEMITRANSPARENCY: u32 = 0x1f19_7698;
const PASS_WATER: u32 = 0x8ef4_0d56;

const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;
const SUB_VIEW_SHADOW_0: u32 = 0x99b2_2d1c;

const GET_NORMAL_MAP: u32 = 0xcbdf_d5ec;
const GET_NORMAL_MAP_ON: u32 = 0xd999_4ef1;
const APPLY_ALPHA_CLIP: u32 = 0xdcfc_844e;
const APPLY_ALPHA_CLIP_ON: u32 = 0x59c4_e6db;
const APPLY_DETAIL_MAP: u32 = 0x6313_fd87;
const APPLY_DETAIL_MAP_ON: u32 = 0x7a3d_9efd;
const APPLY_WAVING_ANIM: u32 = 0x105c_6a52;
const APPLY_WAVING_ANIM_ON: u32 = 0xf801_b859;

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
    set: &[(u32, u32)],
    pass: u32,
    subview: u32,
) -> Option<(u32, u32)> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, material, set)))
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

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).expect("the path list");
    let zone = std::env::args().nth(1).expect("a zone");

    let mut named: BTreeMap<String, Vec<Vec<mtrl::ShaderKey>>> = BTreeMap::new();
    for path in list.lines() {
        if !path.starts_with(&zone) || !path.ends_with(".mtrl") {
            continue;
        }
        let Ok(material) = ironworks.file::<Material>(path) else {
            continue;
        };
        named
            .entry(format!("shader/sm5/shpk/{}", material.shader()))
            .or_default()
            .push(material.shader_keys().to_vec());
    }

    let mut total = (0usize, 0usize);
    for (path, materials) in &named {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            println!("{path}: unread");
            continue;
        };
        let Ok(package) = ShaderPackage::parse(&bytes) else {
            println!("{path}: unparsed");
            continue;
        };
        let head = package.blobs_offset();
        let code = package.bytecode_size();
        let tail = bytes.len() - head - code;

        let mut stages: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for shader in package.shaders() {
            let name = match shader.stage() {
                Stage::Vertex => "vs",
                Stage::Pixel => "ps",
                Stage::Hull => "hs",
                Stage::Domain => "ds",
                Stage::Geometry => "gs",
            };
            let held = stages.entry(name).or_default();
            held.0 += 1;
            held.1 += shader.blob_size() as usize;
        }

        let mut wanted: BTreeSet<u32> = BTreeSet::new();
        for keys in materials {
            for waving in [false, true] {
                let mut set = vec![
                    (GET_NORMAL_MAP, GET_NORMAL_MAP_ON),
                    (APPLY_ALPHA_CLIP, APPLY_ALPHA_CLIP_ON),
                    (APPLY_DETAIL_MAP, APPLY_DETAIL_MAP_ON),
                ];
                if waving {
                    set.push((APPLY_WAVING_ANIM, APPLY_WAVING_ANIM_ON));
                }
                for (pass, subview) in [
                    (PASS_G_OPAQUE, SUB_VIEW_MAIN),
                    (PASS_G_SEMITRANSPARENCY, SUB_VIEW_MAIN),
                    (PASS_Z_OPAQUE, SUB_VIEW_MAIN),
                    (PASS_Z_OPAQUE, SUB_VIEW_SHADOW_0),
                    (PASS_LIGHTING_SEMITRANSPARENCY, SUB_VIEW_MAIN),
                    (PASS_WATER, SUB_VIEW_MAIN),
                ] {
                    if let Some((vs, ps)) = pair(&package, keys, &set, pass, subview) {
                        wanted.insert(vs);
                        wanted.insert(ps);
                    }
                }
            }
        }
        let selected: usize = wanted
            .iter()
            .filter_map(|at| package.shaders().get(*at as usize))
            .map(|shader| shader.blob_size() as usize)
            .sum();
        let span = wanted
            .iter()
            .filter_map(|at| package.shaders().get(*at as usize))
            .map(|shader| {
                (
                    shader.blob_offset() as usize,
                    shader.blob_offset() as usize + shader.blob_size() as usize,
                )
            })
            .fold((usize::MAX, 0usize), |held, one| {
                (held.0.min(one.0), held.1.max(one.1))
            });

        println!(
            "{path}\n  {} materials, {} bytes: head {head}, code {code}, tail {tail}",
            materials.len(),
            bytes.len(),
        );
        let list: Vec<String> = stages
            .iter()
            .map(|(name, (count, size))| format!("{name} {count}/{size}"))
            .collect();
        println!("  stages {}", list.join(", "));
        println!(
            "  selected {} of {} shaders, {selected} bytes, one span {} bytes",
            wanted.len(),
            package.shaders().len(),
            span.1.saturating_sub(span.0.min(span.1)),
        );
        let need = head + tail + selected;
        println!(
            "  would fetch {need} of {} ({:.1}%)",
            bytes.len(),
            100.0 * need as f64 / bytes.len() as f64
        );
        total.0 += bytes.len();
        total.1 += need;
    }
    println!(
        "\ntotal {} -> {} ({:.1}%)",
        total.0,
        total.1,
        100.0 * total.1 as f64 / total.0 as f64
    );
}
