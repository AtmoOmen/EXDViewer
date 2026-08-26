//! Every (vertex, pixel) pair a drawing package's node table can select, translated to GLSL ES
//! 3.00 and run through `glslangValidator`. This is the corpus this repo's own decompiler backend
//! can build; a real driver (ANGLE on D3D11, which Firefox and Edge both use on Windows) validates
//! and compiles far more strictly than the software rasterizers this repo's smoke gates run on, so
//! a failure here can be invisible to both `smoke/native.sh` and `smoke/run.sh`.
//!
//! `shpk_glsl_gate [sqpack dir] [package name ...]`
//!
//! The default package list (31 names) took 17 minutes for `character` alone measured
//! 2026-08-26, so a whole-corpus run is a background job, not a quick check. A single argument is
//! the sqpack dir, not a package name: `shpk_glsl_gate character` reads no such install, sweeps
//! nothing and still exits clean. Name the packages after it: `shpk_glsl_gate
//! /home/asriel/.xlcore/ffxiv/game/sqpack character hair`.

use std::collections::{BTreeSet, HashMap};
use std::panic::{self, AssertUnwindSafe};
use std::process::Command;

use dxbc::chunks::ChunkData;
use ironworks::Ironworks;
use ironworks::file::shpk::{self, ShaderPackage, Stage};
use ironworks::sqpack::{Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// The packages a model can draw with, plus the fixed screen passes the model viewer runs over the
/// whole frame. Read without their bytecode a package still carries its node table, so any of these
/// missing from an install is just skipped, the same as `posteffect_gate`'s read misses.
const NAMES: &[&str] = &[
    "character",
    "characterlegacy",
    "characterglass",
    "characterscroll",
    "characterinc",
    "skin",
    "hair",
    "iris",
    "crystal",
    "apricot_model",
    "apricot_shape",
    "bg",
    "bgprop",
    "bguvscroll",
    "bgcolorchange",
    "cloud",
    "grass",
    "river",
    "water",
    "verticalfog",
    "lightshaft",
    "createviewposition",
    "directionallighting",
    "directionalshadow",
    "pointlighting",
    "spotlighting",
    "linelighting",
    "planelighting",
    "subsurfaceblur",
    "furblur",
    "bg_composite",
];

fn program(bytes: &[u8], package: &ShaderPackage, index: u32) -> Option<dxbc::shex::Program> {
    let shader = package.shaders().get(index as usize)?;
    let start = package.blobs_offset() + usize::try_from(shader.blob_offset()).ok()?;
    let end = start.checked_add(usize::try_from(shader.blob_size()).ok()?)?;
    let blob = bytes.get(start..end)?;
    dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .find_map(|chunk| match chunk.parse() {
            ChunkData::Shader(program) => Some(program),
            _ => None,
        })
}

fn names(package: &ShaderPackage, bytes: &[u8], index: u32) -> hlsl::Names {
    let mut names = hlsl::Names::default();
    let Some(shader) = package.shaders().get(index as usize) else {
        return names;
    };
    let label = |resource: &shpk::Resource| {
        package
            .name(resource)
            .map(str::to_owned)
            .or_else(|| shaders::names::resolve(resource.id()).map(str::to_owned))
    };
    for resource in shader.textures() {
        if let Some(name) = label(resource) {
            names.textures.insert(resource.slot(), name);
        }
    }
    for resource in shader.samplers() {
        if let Some(name) = label(resource) {
            names.samplers.insert(resource.slot(), name);
        }
    }
    for resource in shader.constants() {
        if let Some(name) = label(resource) {
            names
                .constants
                .insert(resource.slot(), hlsl::Buffer::new(name, Vec::new()));
        }
    }
    let start = package.blobs_offset() + shader.blob_offset() as usize;
    let end = start + shader.blob_size() as usize;
    let Some(blob) = bytes.get(start..end) else {
        return names;
    };
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

/// Every (vertex, pixel) pair a package's node table can select, absolute indices into its own
/// shader list, deduplicated: translation only depends on the pair, never on which node reached it.
fn pairs(package: &ShaderPackage) -> BTreeSet<(u32, u32)> {
    let base = |want: Stage| {
        package
            .shaders()
            .iter()
            .take_while(|shader| shader.stage() != want)
            .count() as u32
    };
    let (vs_base, ps_base) = (base(Stage::Vertex), base(Stage::Pixel));
    package
        .nodes()
        .iter()
        .flat_map(shpk::Node::passes)
        .filter(|pass| pass.vertex() != shpk::NONE && pass.pixel() != shpk::NONE)
        .map(|pass| (vs_base + pass.vertex(), ps_base + pass.pixel()))
        .collect()
}

/// One page of a pixel shader's outputs, the same split `mdl/program.rs::assemble` makes for a
/// context with fewer draw buffers than the shader has targets.
fn pages(outputs: &[u32], attachments: usize) -> Vec<Vec<u32>> {
    if outputs.is_empty() {
        return vec![Vec::new()];
    }
    outputs
        .chunks(attachments.max(1))
        .map(<[u32]>::to_vec)
        .collect()
}

fn validate(stage: &str, source: &str) -> Option<String> {
    let extension = match stage {
        "vs" => "vert",
        _ => "frag",
    };
    let path =
        std::env::temp_dir().join(format!("shpk_glsl_gate.{}.{extension}", std::process::id()));
    std::fs::write(&path, source).ok()?;
    let output = Command::new("glslangValidator")
        .arg("-S")
        .arg(extension)
        .arg(&path)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&path);
    match output.status.success() {
        true => None,
        false => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
    }
}

fn main() {
    let sqpack = std::env::args().nth(1).unwrap_or_else(|| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));

    let asked: Vec<String> = std::env::args().skip(2).collect();
    let wanted: &[String] = match asked.is_empty() {
        true => &NAMES
            .iter()
            .map(|held| (*held).to_owned())
            .collect::<Vec<_>>()
            .leak(),
        false => &asked,
    };

    let has_validator = Command::new("glslangValidator")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !has_validator {
        println!("glslangValidator not found on PATH; translating only, not validating");
    }

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));

    let (mut read_ok, mut read_missed) = (0usize, 0usize);
    let mut pairs_seen = 0usize;
    let mut translate_failed: Vec<(String, u32, u32, String)> = Vec::new();
    let mut translate_panicked: Vec<(String, u32, u32, String)> = Vec::new();
    let mut validate_failed: Vec<(String, u32, u32, &str, String)> = Vec::new();
    let mut validated_ok = 0usize;

    for name in wanted {
        let path = format!("shader/sm5/shpk/{name}.shpk");
        let bytes: Vec<u8> = match ironworks.file(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                read_missed += 1;
                continue;
            }
        };
        let package = match ShaderPackage::parse(&bytes) {
            Ok(package) => package,
            Err(_) => {
                read_missed += 1;
                continue;
            }
        };
        read_ok += 1;

        for (vs, ps) in pairs(&package) {
            pairs_seen += 1;
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                let vertex = program(&bytes, &package, vs)
                    .ok_or_else(|| "no vertex shader in the blob".to_owned())?;
                let fragment = program(&bytes, &package, ps)
                    .ok_or_else(|| "no pixel shader in the blob".to_owned())?;
                let vs_names = names(&package, &bytes, vs);
                let ps_names = names(&package, &bytes, ps);

                let mut extents: HashMap<String, u32> = hlsl::glsl::extents(&vertex, &vs_names);
                for (name, registers) in hlsl::glsl::extents(&fragment, &ps_names) {
                    let held = extents.entry(name).or_insert(0);
                    *held = (*held).max(registers);
                }
                let outputs: Vec<u32> = {
                    let mut held: Vec<u32> = ps_names.outputs.keys().copied().collect();
                    held.sort_unstable();
                    held
                };

                let mut built = Vec::new();
                for &attachments in &[4usize, 8usize] {
                    for held in pages(&outputs, attachments) {
                        let vs_options = hlsl::glsl::Options {
                            targets: Vec::new(),
                            extents: extents.clone(),
                        };
                        let ps_options = hlsl::glsl::Options {
                            targets: held,
                            extents: extents.clone(),
                        };
                        let vertex_src =
                            hlsl::glsl(&vertex, &vs_names, hlsl::Reading::Plain, &vs_options)
                                .lines
                                .join("\n");
                        let fragment_src =
                            hlsl::glsl(&fragment, &ps_names, hlsl::Reading::Plain, &ps_options)
                                .lines
                                .join("\n");
                        built.push((vertex_src, fragment_src));
                    }
                }
                Ok::<_, String>(built)
            }));

            match result {
                Ok(Ok(mut built)) => {
                    built.dedup();
                    for (vertex_src, fragment_src) in built {
                        if !has_validator {
                            continue;
                        }
                        if let Some(why) = validate("vs", &vertex_src) {
                            validate_failed.push((name.clone(), vs, ps, "vertex", why));
                        } else {
                            validated_ok += 1;
                        }
                        if let Some(why) = validate("ps", &fragment_src) {
                            validate_failed.push((name.clone(), vs, ps, "fragment", why));
                        } else {
                            validated_ok += 1;
                        }
                    }
                }
                Ok(Err(why)) => translate_failed.push((name.clone(), vs, ps, why)),
                Err(panic) => {
                    let message = panic
                        .downcast_ref::<&str>()
                        .map(|held| (*held).to_owned())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "panicked".to_owned());
                    translate_panicked.push((name.clone(), vs, ps, message));
                }
            }
        }
    }

    panic::set_hook(previous_hook);

    println!("read {read_ok} packages ({read_missed} missing or unparsable)");
    println!("{pairs_seen} distinct (vertex, pixel) pairs across them");
    println!(
        "translate: {} failed, {} panicked",
        translate_failed.len(),
        translate_panicked.len()
    );
    for (name, vs, ps, why) in &translate_failed {
        println!("  {name} vs{vs}/ps{ps}: {why}");
    }
    for (name, vs, ps, why) in &translate_panicked {
        println!("  {name} vs{vs}/ps{ps}: panicked: {why}");
    }
    if has_validator {
        println!(
            "glslangValidator: {validated_ok} ok, {} failed",
            validate_failed.len()
        );
        for (name, vs, ps, stage, why) in &validate_failed {
            println!("  {name} vs{vs}/ps{ps} ({stage}):");
            for line in why.lines() {
                println!("    {line}");
            }
        }
    }
}
