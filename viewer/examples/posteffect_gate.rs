//! How many of the game's post-effect shaders still translate and compile to GLSL ES 3.00,
//! measured directly against the install rather than through the viewer's UI.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};

use dxbc::chunks::ChunkData;
use ironworks::Ironworks;
use ironworks::file::shcd::{self, Stage};
use ironworks::sqpack::{Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const NAMES: &[&str] = &[
    "ToneMapping",
    "ToneMapLut",
    "ToneAdjust",
    "ColorFilter",
    "Saturate",
    "AdaptLum",
    "MeasureLumInitial",
    "MeasureLumIterative",
    "MeasureLumFinal",
    "BrightPassFilter",
    "BloomBlur_Linear",
    "GlareMerge",
    "Halo",
    "StarBlur",
    "FXAA",
    "FXAALuma",
    "ssao",
    "SSAO1",
    "SSAO2",
    "SSAO3",
    "SSAO4",
    "SSAO5",
    "SSAO6",
    "SSAO7",
    "SSAO8",
    "Fog",
    "heightfog",
    "UnderWaterFog",
    "Sky",
    "Sky2",
    "Sun",
    "Moon",
    "CloudShadow",
    "GodraysMerge",
    "skyocclusion",
    "DownScale3x3",
    "DownScale4x4",
    "Vignetting",
    "srgbtolinear",
    "lineartosrgb",
    "decodehdr",
    "CameraMotionBlur",
    "CameraVelocity",
    "RadialBlur",
    "circleofconfusion",
    "DonutMerge",
    // One bounded pass of guesses at stems the corpus is likely to also carry. sqpack has no
    // directory listing, so a miss (NotFound) is the only way to find out; it costs nothing and is
    // already counted as a read failure.
    "BloomBlur_Point",
    "Downsample",
    "DownSample",
    "Fog2",
    "Sun2",
    "Moon2",
    "GlareLuminance",
    "Blur",
    "DoF",
    "DepthOfField",
    "Bokeh",
    "Distortion",
    "Refraction",
    "WaterRefraction",
    "Lensflare",
    "LensFlare",
    "Ripple",
    "Wave",
    "Blend",
    "Copy",
    "Clear",
    "SMAA",
    "SMAALuma",
    "SMAABlend",
    "SMAANeighborhood",
    "GodraysGenerate",
    "Godrays",
    "SkyDistant",
    "LightShaft",
    "Water",
];

fn program(blob: &[u8]) -> Option<dxbc::shex::Program> {
    dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .find_map(|chunk| match chunk.parse() {
            ChunkData::Shader(program) => Some(program),
            _ => None,
        })
}

/// What this shader's registers are called, so the reading names them rather than their slots.
fn names(code: &shcd::ShaderCode, blob: &[u8]) -> hlsl::Names {
    let mut names = hlsl::Names::default();
    for resource in code.textures() {
        if let Some(name) = code.name(resource) {
            names.textures.insert(resource.slot(), name.to_owned());
        }
    }
    for resource in code.samplers() {
        if let Some(name) = code.name(resource) {
            names.samplers.insert(resource.slot(), name.to_owned());
        }
    }
    for resource in code.constants() {
        if let Some(name) = code.name(resource) {
            names.constants.insert(
                resource.slot(),
                hlsl::Buffer::new(name.to_owned(), Vec::new()),
            );
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

fn main() {
    let sqpack = std::env::args().nth(1).unwrap_or_else(|| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));

    let out_dir = std::path::Path::new("/tmp/posteffect");
    std::fs::create_dir_all(out_dir).expect("create output dir");

    // The decompiler is not expected to panic anymore, but the whole point of this harness is to
    // find out, so failures are caught rather than left to crash the run; the default hook would
    // otherwise spam a backtrace to stderr for every one of them.
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));

    let mut read_ok = 0usize;
    let mut read_failed: Vec<(&str, String)> = Vec::new();
    let mut translated: Vec<&str> = Vec::new();
    let mut translate_failed: Vec<(&str, String)> = Vec::new();

    let asked: Vec<&'static str> = std::env::args()
        .skip(2)
        .map(|name| &*Box::leak(name.into_boxed_str()))
        .collect();
    let wanted: &[&str] = match asked.is_empty() {
        true => NAMES,
        false => Box::leak(asked.into_boxed_slice()),
    };

    for &name in wanted {
        let path = format!("shader/sm5/posteffect/{name}.shcd");
        let bytes: Vec<u8> = match ironworks.file(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                read_failed.push((name, error.to_string()));
                continue;
            }
        };
        let code = match shcd::ShaderCode::parse(&bytes) {
            Ok(code) => code,
            Err(error) => {
                read_failed.push((name, error.to_string()));
                continue;
            }
        };
        read_ok += 1;

        let blob_range = code.blob_offset()..code.blob_offset() + code.blob_size();
        let Some(blob) = bytes.get(blob_range) else {
            translate_failed.push((name, "blob out of range".to_owned()));
            continue;
        };
        let Some(dx_program) = program(blob) else {
            translate_failed.push((name, "no shader program in blob".to_owned()));
            continue;
        };
        let shader_names = names(&code, blob);
        let stage = match code.stage() {
            Stage::Vertex => "Vertex",
            Stage::Pixel => "Pixel",
            Stage::Geometry => "Geometry",
            Stage::Compute => "Compute",
            Stage::Hull => "Hull",
            Stage::Domain => "Domain",
            Stage::Unknown(_) => "Unknown",
        };

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let extents: HashMap<String, u32> = hlsl::glsl::extents(&dx_program, &shader_names);
            let is_vertex = dx_program.shader_type == "vs";
            let mut targets: Vec<u32> = match is_vertex {
                true => Vec::new(),
                false => shader_names.outputs.keys().copied().collect(),
            };
            targets.sort_unstable();
            let options = hlsl::glsl::Options { targets, extents };
            hlsl::glsl(&dx_program, &shader_names, hlsl::Reading::Plain, &options)
        }));

        match result {
            Ok(decompiled) => {
                let extension = match dx_program.shader_type {
                    "vs" => "vert",
                    _ => "frag",
                };
                let out_path = out_dir.join(format!("{name}.{extension}.glsl"));
                std::fs::write(&out_path, decompiled.lines.join("\n")).expect("write output");
                let uses_index_range = dx_program.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        dxbc::shex::InstructionKind::DclIndexRange { .. }
                    )
                });
                if uses_index_range {
                    println!(
                        "  {name}: uses dcl_indexRange, reading.indexed={} dropped={:?}",
                        decompiled.indexed, decompiled.dropped
                    );
                }
                translated.push(name);
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|held| (*held).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panicked".to_owned());
                translate_failed.push((
                    name,
                    format!("{stage} ({}): {message}", dx_program.shader_type),
                ));
            }
        }
    }

    panic::set_hook(previous_hook);

    println!(
        "read {}/{} ({} failed to read)",
        read_ok,
        NAMES.len(),
        read_failed.len()
    );
    for (name, error) in &read_failed {
        println!("  read failed: {name}: {error}");
    }
    println!("translated {}/{}", translated.len(), read_ok);
    for (name, error) in &translate_failed {
        println!("  translate failed: {name}: {error}");
    }
    println!("output written to {}", out_dir.display());
}
