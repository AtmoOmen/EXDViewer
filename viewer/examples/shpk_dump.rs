//! Dump a shader package whole: keys, nodes, passes, and every shader's resources and HLSL.
//!
//! `shpk_dump <path.shpk> [shader index]`

use dxbc::chunks::ChunkData;
use ironworks::{
    Ironworks,
    file::shpk::{self, ShaderPackage, Stage},
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn named(id: u32) -> String {
    shaders::names::resolve(id).map_or_else(|| format!("{id:08x}"), ToOwned::to_owned)
}

fn label(package: &ShaderPackage, resource: &shpk::Resource) -> String {
    package
        .name(resource)
        .map(str::to_owned)
        .or_else(|| shaders::names::resolve(resource.id()).map(str::to_owned))
        .unwrap_or_else(|| format!("{:08x}", resource.id()))
}

fn names(package: &ShaderPackage, index: usize, blob: &[u8]) -> hlsl::Names {
    let mut names = hlsl::Names::default();
    let shader = &package.shaders()[index];
    for resource in shader.textures() {
        names
            .textures
            .insert(resource.slot(), label(package, resource));
    }
    for resource in shader.samplers() {
        names
            .samplers
            .insert(resource.slot(), label(package, resource));
    }
    for resource in shader.constants() {
        names.constants.insert(
            resource.slot(),
            hlsl::Buffer::new(label(package, resource), Vec::new()),
        );
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

fn main() {
    let ironworks =
        Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK.to_owned())));
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a .shpk path");
    let only: Option<usize> = args.next().and_then(|held| held.parse().ok());

    let raw: Vec<u8> = ironworks.file(&path).expect("package");
    let package = ShaderPackage::parse(&raw).expect("package");

    println!("== {path}");
    println!("version {:#06x} {:?}", package.version(), package.directx());
    let [technique, subview] = package.technique_subview();
    println!("technique {technique:08x} subview {subview:08x}");
    println!("param buffer {} bytes", package.param_buffer_size());
    for param in package.material_params() {
        println!(
            "  param {} at +{} ({} B) default {:?}",
            named(param.id()),
            param.byte_offset(),
            param.byte_size(),
            package.param_default(param),
        );
    }
    for (kind, keys) in [
        ("system", package.system_keys()),
        ("scene", package.scene_keys()),
        ("material", package.material_keys()),
    ] {
        for key in keys {
            println!(
                "{kind} key {} ({:08x}) default {} ({:08x})",
                named(key.id()),
                key.id(),
                named(key.default_value()),
                key.default_value()
            );
        }
    }
    for (which, list) in [
        ("constant", package.constants()),
        ("sampler", package.samplers()),
        ("texture", package.textures()),
        ("uav", package.uavs()),
    ] {
        for resource in list {
            println!(
                "package {which} {} ({:08x}) slot {} size {}",
                label(&package, resource),
                resource.id(),
                resource.slot(),
                resource.size(),
            );
        }
    }
    for held in package.nodes() {
        println!(
            "node {:08x} keys {:?} passes [{}]",
            held.id(),
            held.keys()
                .iter()
                .map(|key| format!("{}({key:08x})", named(*key)))
                .collect::<Vec<_>>(),
            held.passes()
                .iter()
                .map(|pass| format!(
                    "{}({:08x}, vs {} ps {})",
                    named(pass.id()),
                    pass.id(),
                    pass.vertex(),
                    pass.pixel()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for alias in package.aliases() {
        println!("alias {:08x} -> node {}", alias.selector(), alias.node());
    }

    for (index, shader) in package.shaders().iter().enumerate() {
        if only.is_some_and(|want| want != index) {
            continue;
        }
        let stage = shader.stage();
        let within = package
            .shaders()
            .iter()
            .take(index)
            .filter(|other| other.stage() == stage)
            .count();
        println!(
            "\n// ===== shader {index} ({stage:?} #{within}) {} bytes",
            shader.blob_size()
        );
        for resource in shader.resources() {
            println!(
                "//   {} ({:08x}) slot {} size {}",
                label(&package, resource),
                resource.id(),
                resource.slot(),
                resource.size()
            );
        }
        let start = package.blobs_offset() + shader.blob_offset() as usize;
        let blob = &raw[start..start + shader.blob_size() as usize];
        for chunk in dxbc::scan_dxbc(blob)
            .iter()
            .flat_map(|container| &container.chunks)
        {
            match chunk.parse() {
                ChunkData::Rdef(rdef) => {
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
                    for binding in &rdef.bindings {
                        println!(
                            "// binding {} type {:?} dim {:?} point {} count {}",
                            binding.name,
                            binding.input_type,
                            binding.dimension,
                            binding.bind_point,
                            binding.bind_count
                        );
                    }
                }
                ChunkData::InputSignature(signature) => {
                    for element in &signature.elements {
                        println!(
                            "// in  {}{} reg {} mask {:#x}",
                            element.semantic_name,
                            element.semantic_index,
                            element.register,
                            element.mask
                        );
                    }
                }
                ChunkData::OutputSignature(signature) => {
                    for element in &signature.elements {
                        println!(
                            "// out {}{} reg {} mask {:#x}",
                            element.semantic_name,
                            element.semantic_index,
                            element.register,
                            element.mask
                        );
                    }
                }
                _ => {}
            }
        }
        let program = dxbc::scan_dxbc(blob)
            .iter()
            .flat_map(|container| &container.chunks)
            .find_map(|chunk| match chunk.parse() {
                ChunkData::Shader(program) => Some(program),
                _ => None,
            });
        match program {
            Some(program) => {
                let held = names(&package, index, blob);
                // The same shader the way the viewer links it, where asked: the two backends read
                // one expression tree, and only this one says what a draw really runs.
                let lines = match std::env::var("GLSL").is_ok() {
                    true => {
                        let mut targets: Vec<u32> = match stage == Stage::Vertex {
                            true => Vec::new(),
                            false => held.outputs.keys().copied().collect(),
                        };
                        targets.sort_unstable();
                        let options = hlsl::glsl::Options {
                            targets,
                            extents: hlsl::glsl::extents(&program, &held),
                        };
                        hlsl::glsl(&program, &held, hlsl::Reading::Plain, &options).lines
                    }
                    false => hlsl::decompile(&program, &held).lines,
                };
                println!("{}", lines.join("\n"));
            }
            None => println!("// no shex chunk"),
        }
    }
    let _ = Stage::Vertex;
}
