//! Dump a shader code file: its resources, its buffer layouts and its HLSL, or its GLSL under
//! `GLSL=1`. A file whose name the path list does not know is reached by the hash its directory
//! records it under, the same way the asset browser reaches one.
//!
//! `shcd_dump shader/sm5/posteffect/Fog.shcd`
//! `shcd_dump shader/sm5/posteffect/e8bf3721`

use std::io::{Read, Seek};

use dxbc::chunks::ChunkData;
use ironworks::file::shcd::{self, ShaderCode, Stage};
use ironworks::sqpack::{IndexHash, Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn label(code: &ShaderCode, resource: &shcd::Resource) -> String {
    code.name(resource)
        .map(str::to_owned)
        .or_else(|| shaders::names::resolve(resource.id()).map(str::to_owned))
        .unwrap_or_else(|| format!("{:08x}", resource.id()))
}

fn names(code: &ShaderCode, blob: &[u8]) -> hlsl::Names {
    let mut names = hlsl::Names::default();
    for resource in code.textures() {
        names
            .textures
            .insert(resource.slot(), label(code, resource));
    }
    for resource in code.samplers() {
        names
            .samplers
            .insert(resource.slot(), label(code, resource));
    }
    for resource in code.constants() {
        names.constants.insert(
            resource.slot(),
            hlsl::Buffer::new(label(code, resource), Vec::new()),
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

fn read<R: Read + Seek>(mut file: ironworks::sqpack::File<R>) -> Vec<u8> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("the file");
    bytes
}

/// The file's bytes, by path where the install knows the name and by the directory's own record
/// where the last segment is the hash the name would have had.
fn bytes(sqpack: &SqPack<Install>, path: &str) -> Vec<u8> {
    let (repository, category) = sqpack.locate(path).expect("a shader path");
    if let Ok(file) = sqpack.file(path) {
        return read(file);
    }
    let (directory, name) = path.rsplit_once('/').expect("a directory");
    let hash = u32::from_str_radix(name, 16).expect("a name or a hash");
    let (Some(IndexHash::Split(whole)), _) = IndexHash::of(&format!("{directory}/x")) else {
        panic!("no split hash for {directory}");
    };
    let held = IndexHash::Split(whole & !0xffff_ffff | u64::from(hash));
    read(
        sqpack
            .file_by_hash(repository, category, held)
            .expect("the file"),
    )
}

fn main() {
    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    for path in std::env::args().skip(1) {
        let raw = bytes(&sqpack, &path);
        let code = ShaderCode::parse(&raw).expect("shader code");
        println!("== {path}");
        println!(
            "version {:#06x} {:?} {:?}  {} bytes",
            code.version(),
            code.directx(),
            code.stage(),
            code.blob_size()
        );
        for resource in code.resources() {
            println!(
                "//   {} ({:08x}) slot {} size {}",
                label(&code, resource),
                resource.id(),
                resource.slot(),
                resource.size()
            );
        }
        let blob = &raw[code.blob_offset()..code.blob_offset() + code.blob_size()];
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
                            "// binding {} type {:?} point {} count {}",
                            binding.name,
                            binding.input_type,
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
            })
            .expect("a shader chunk");
        let held = names(&code, blob);
        let lines = match std::env::var("GLSL").is_ok() {
            true => {
                let mut targets: Vec<u32> = match code.stage() == Stage::Vertex {
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
        for line in lines {
            println!("{line}");
        }
    }
}
