//! Which shaders a package or a model's materials resolve to, and what one of them decompiles to.
//!
//! `shader_node <path.mdl>` walks the model's materials and prints the node each selects, once with
//! the keys the file states and once with the keys the zone viewer sets over them.
//!
//! `shader_node <path.shpk> <PASS_NAME> [vs|ps] [path.mtrl]` prints the package's pass tally, its material
//! parameter offsets, the node the viewer would take, that shader's constant buffer layouts as its
//! own reflection describes them, and the shader as HLSL.

use dxbc::chunks::ChunkData;
use ironworks::{
    Ironworks,
    file::{
        mdl::{Lod, ModelContainer},
        mtrl::{Material, ShaderKey},
        shpk::{self, ShaderPackage, Stage},
    },
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;

/// The keys the engine sets rather than a material, as `layer/scene/mod.rs` and `mdl/program.rs`
/// set them. A package that declares none of them resolves exactly as it would without.
const KEYS: [(u32, u32); 6] = [
    (0x6313_fd87, 0x7a3d_9efd),
    (0x8115_916d, 0x51ed_d496),
    (0x0d81_2fa4, 0xaba1_f498),
    (0xcbdf_d5ec, 0xd999_4ef1),
    (0xdcfc_844e, 0x59c4_e6db),
    (0x1143_3f2d, 0x4ba7_7904),
];

fn named(id: u32) -> String {
    shaders::names::resolve(id).map_or_else(|| format!("{id:08x}"), ToOwned::to_owned)
}

fn selector(keys: &[u32]) -> u32 {
    let (mut out, mut mul) = (0u32, 1u32);
    for key in keys {
        out = out.wrapping_add(key.wrapping_mul(mul));
        mul = mul.wrapping_mul(31);
    }
    out
}

fn values(keys: &[shpk::Key], material: &[ShaderKey], set: &[(u32, u32)]) -> Vec<u32> {
    keys.iter()
        .map(|key| {
            set.iter()
                .find(|(id, _)| *id == key.id())
                .map(|(_, value)| *value)
                .or_else(|| {
                    material
                        .iter()
                        .find(|held| held.category() == key.id())
                        .map(ShaderKey::value)
                })
                .unwrap_or_else(|| key.default_value())
        })
        .collect()
}

fn node<'a>(
    package: &'a ShaderPackage,
    material: &[ShaderKey],
    set: &[(u32, u32)],
) -> Option<&'a shpk::Node> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, material, set)))
    .collect();
    parts.push(selector(&[package.technique_subview()[0], SUB_VIEW_MAIN]));
    let id = selector(&parts);
    package
        .nodes()
        .iter()
        .find(|node| node.id() == id)
        .or_else(|| {
            let alias = package
                .aliases()
                .iter()
                .find(|alias| alias.selector() == id)?;
            package.nodes().get(alias.node() as usize)
        })
}

fn passes(held: &shpk::Node) -> String {
    held.passes()
        .iter()
        .map(|pass| {
            format!(
                "{}(vs {} ps {})",
                named(pass.id()),
                pass.vertex(),
                pass.pixel()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn shex(blob: &[u8]) -> Option<dxbc::shex::Program> {
    dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .find_map(|chunk| match chunk.parse() {
            ChunkData::Shader(program) => Some(program),
            _ => None,
        })
}

fn names(package: &ShaderPackage, index: u32, blob: &[u8]) -> hlsl::Names {
    let mut names = hlsl::Names::default();
    let Some(shader) = package.shaders().get(index as usize) else {
        return names;
    };
    let name = |resource: &shpk::Resource| {
        package
            .name(resource)
            .map(str::to_owned)
            .or_else(|| shaders::names::resolve(resource.id()).map(str::to_owned))
    };
    for resource in shader.textures() {
        if let Some(held) = name(resource) {
            names.textures.insert(resource.slot(), held);
        }
    }
    for resource in shader.samplers() {
        if let Some(held) = name(resource) {
            names.samplers.insert(resource.slot(), held);
        }
    }
    for resource in shader.constants() {
        if let Some(held) = name(resource) {
            names
                .constants
                .insert(resource.slot(), hlsl::Buffer::new(held, Vec::new()));
        }
    }
    for chunk in dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
    {
        let (into, signature) = match chunk.parse() {
            ChunkData::InputSignature(signature) => (&mut names.inputs, signature),
            ChunkData::OutputSignature(signature) => (&mut names.outputs, signature),
            _ => continue,
        };
        for element in &signature.elements {
            into.entry(element.register).or_insert_with(|| {
                hlsl::Semantic::new(
                    &element.semantic_name,
                    element.semantic_index,
                    element.component_type,
                    element.mask,
                )
            });
        }
    }
    names
}

/// A pass names a shader by its index within its own stage, not within the whole list.
fn base(package: &ShaderPackage, want: Stage) -> u32 {
    package
        .shaders()
        .iter()
        .take_while(|shader| shader.stage() != want)
        .count() as u32
}

fn material(ironworks: &Ironworks<SqPack<Install>>, path: &str) {
    let material = match ironworks.file::<Material>(path) {
        Ok(material) => material,
        Err(error) => return println!("  {path}: {error}"),
    };
    println!("  material {path}");
    println!("    package  {}", material.shader());
    for key in material.shader_keys() {
        println!(
            "    key      {} = {}",
            named(key.category()),
            named(key.value())
        );
    }
    for constant in material.constants() {
        let values = material.constant_values(constant).unwrap_or_default();
        println!("    const    {} = {values:?}", named(constant.id()));
    }
    for sampler in material.samplers() {
        println!(
            "    sampler  {} flags {:#x}",
            named(sampler.id()),
            sampler.flags()
        );
    }
    for texture in material.textures() {
        println!("    texture  {}", texture.path());
    }

    let path = format!("shader/sm5/shpk/{}", material.shader());
    let Ok(raw) = ironworks.file::<Vec<u8>>(&path) else {
        return println!("    no {path}");
    };
    let Ok(package) = ShaderPackage::parse(&raw) else {
        return;
    };
    for (label, set) in [("as shipped", &[][..]), ("with the engine keys", &KEYS[..])] {
        match node(&package, material.shader_keys(), set) {
            Some(held) => println!("    node {label}: {}", passes(held)),
            None => println!("    node {label}: none"),
        }
    }
}

fn main() {
    let ironworks =
        Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK.to_owned())));
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a .mdl or .shpk path");

    if path.ends_with(".mdl") {
        let container = ironworks.file::<ModelContainer>(&path).expect("model");
        println!("== {path}");
        let mut seen: Vec<String> = Vec::new();
        for mesh in container.model(Lod::High).meshes() {
            let name = mesh.material().expect("material").to_string();
            if seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            // A background model names its materials relative to its own directory's parent.
            let resolved = match (name.strip_prefix('/'), path.rsplit_once("/bgparts/")) {
                (Some(rest), Some((root, _))) => format!("{root}/material/{rest}"),
                _ => name,
            };
            material(&ironworks, &resolved);
        }
        return;
    }

    let raw: Vec<u8> = ironworks.file(&path).expect("package");
    let package = ShaderPackage::parse(&raw).expect("package");
    let want = shaders::names::hash(args.next().expect("a pass name").as_bytes());
    let stage = match args.next().as_deref() {
        Some("vs") => Stage::Vertex,
        _ => Stage::Pixel,
    };

    let mut tally = std::collections::BTreeMap::new();
    for held in package.nodes() {
        for pass in held.passes() {
            *tally.entry(named(pass.id())).or_insert(0usize) += 1;
        }
    }
    println!("// {} nodes", package.nodes().len());
    for (name, count) in tally {
        println!("//   {name} {count}");
    }
    for param in package.material_params() {
        let lane = b"xyzw"[(param.byte_offset() as usize % 16) / 4] as char;
        println!(
            "// material param {} at +{} ({} B) reg {}.{lane}",
            named(param.id()),
            param.byte_offset(),
            param.byte_size(),
            param.byte_offset() / 16,
        );
    }

    let keys = args
        .next()
        .and_then(|path| ironworks.file::<Material>(&path).ok());
    let held = node(
        &package,
        keys.as_ref().map(Material::shader_keys).unwrap_or(&[]),
        &KEYS,
    )
    .expect("node");
    println!("// node {:08x}: {}", held.id(), passes(held));
    let selected = held
        .passes()
        .iter()
        .find(|held| held.id() == want)
        .expect("pass");
    let index = base(&package, stage)
        + match stage {
            Stage::Vertex => selected.vertex(),
            _ => selected.pixel(),
        };

    let shader = package.shaders().get(index as usize).expect("shader");
    let start = package.blobs_offset() + shader.blob_offset() as usize;
    let blob = &raw[start..start + shader.blob_size() as usize];
    for chunk in dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
    {
        if let ChunkData::Rdef(rdef) = chunk.parse() {
            for buffer in &rdef.constant_buffers {
                println!("// cbuffer {} size {}", buffer.name, buffer.size);
                for member in hlsl::layout::members(buffer) {
                    println!(
                        "//   +{:<4} ({:>3} B) reg {:<3} {} {}",
                        member.offset,
                        member.size,
                        member.offset / 16,
                        member.kind,
                        member.name
                    );
                }
            }
        }
    }
    let program = shex(blob).expect("program");
    println!(
        "{}",
        hlsl::decompile(&program, &names(&package, index, blob))
            .lines
            .join("\n")
    );
}
