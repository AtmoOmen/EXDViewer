//! `.shpk` shader packages: the compiled shaders a material names, and the resources, parameters
//! and keys they are driven by.

mod keys;
mod list;
mod merged;
mod params;

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use egui::ScrollArea;
use hlsl::layout::Member;
use ironworks::file::shpk::{self, Stage};
use shaders::names;

use super::shader::{self, Naming, ResourceRow, Shader};
use super::{Preview, facts, section};
use crate::assets::Bytes;
use crate::utils::export;
use keys::Keys;
use params::{COMPONENTS, Component, ParamRow};

/// A shader package, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    /// Resources under the heading each group is drawn with.
    resources: Vec<(&'static str, Vec<ResourceRow>)>,
    params: Vec<ParamRow>,
    /// The parameter buffer as the registers a shader addresses it by.
    registers: Vec<[Option<Component>; COMPONENTS]>,
    /// The shader's defaults for that buffer, indexed by the same float.
    defaults: Vec<f32>,
    keys: Keys,
    /// Stage, how many shaders it holds, and how much bytecode they take.
    stages: Vec<(&'static str, usize, usize)>,
    shaders: Vec<Shader>,
    naming: Naming,
    /// Which stage is filtered to and which shader is picked, kept per file the way the icon sheet
    /// keeps its controller.
    state: egui::Id,
}

/// Constant buffer layouts, by the resource id that names the buffer.
///
/// The layouts live in the compiled bytecode's reflection rather than in the package tables, and
/// every shader that binds a buffer describes it identically. So rather than sweeping thousands of
/// blobs, this walks the shader list once and takes only those that bind a buffer nothing before
/// them did: enough to cover every declared buffer, in around ten blobs even for the largest
/// package.
fn layouts(package: &shpk::ShaderPackage, bytes: &[u8]) -> HashMap<u32, Vec<Member>> {
    let wanted: HashSet<u32> = package.constants().iter().map(|c| c.id()).collect();
    let mut seen = HashSet::new();
    let mut found = HashMap::new();

    for shader in package.shaders() {
        if seen.len() == wanted.len() {
            break;
        }
        let adds = shader
            .resources()
            .iter()
            .any(|resource| wanted.contains(&resource.id()) && seen.insert(resource.id()));
        if !adds {
            continue;
        }

        let start = package.blobs_offset() + usize::try_from(shader.blob_offset()).unwrap_or(0);
        let end = start.saturating_add(usize::try_from(shader.blob_size()).unwrap_or(0));
        if let Some(blob) = bytes.get(start..end) {
            shader::buffers(blob, &mut found);
        }
    }
    found
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    // Parsed off the caller's bytes rather than an owned copy
    let package = shpk::ShaderPackage::parse(bytes)?;
    let layouts = layouts(&package, bytes);

    let mut stages: Vec<(&'static str, usize, usize)> = Vec::new();
    let mut shaders = Vec::with_capacity(package.shaders().len());
    for shader in package.shaders() {
        let stage = match shader.stage() {
            Stage::Vertex => "Vertex",
            Stage::Pixel => "Pixel",
            Stage::Hull => "Hull",
            Stage::Domain => "Domain",
            Stage::Geometry => "Geometry",
        };
        let size = usize::try_from(shader.blob_size()).unwrap_or(0);
        let start = package.blobs_offset() + usize::try_from(shader.blob_offset()).unwrap_or(0);
        shaders.push(Shader {
            stage,
            blob: start..start.saturating_add(size),
            bindings: shader::bindings(shader.constants(), shader.samplers(), shader.textures()),
        });
        match stages.iter_mut().find(|(name, _, _)| *name == stage) {
            Some((_, count, bytes)) => {
                *count += 1;
                *bytes += size;
            }
            None => stages.push((stage, 1, size)),
        }
    }

    let resources = shader::resources(
        [
            package.constants(),
            package.samplers(),
            package.textures(),
            package.uavs(),
        ],
        |resource| package.name(resource),
        &layouts,
    );

    let params = params::rows(&package);
    let registers = params::registers(&package);

    let [technique, subview] = package.technique_subview();
    let identity = vec![
        ("Version", format!("{:#06X}", package.version())),
        (
            "DirectX",
            match package.directx() {
                shpk::DirectX::Dx9 => "9".to_owned(),
                shpk::DirectX::Dx11 => "11".to_owned(),
                shpk::DirectX::Unknown(tag) => String::from_utf8_lossy(&tag).into_owned(),
            },
        ),
        ("Shaders", package.shaders().len().to_string()),
        ("Bytecode", Bytes(package.bytecode_size()).to_string()),
        (
            "Parameter buffer",
            format!(
                "{} registers ({} B)",
                registers.len(),
                package.param_buffer_size()
            ),
        ),
        ("Selector nodes", package.nodes().len().to_string()),
        ("Aliases", package.aliases().len().to_string()),
        ("Technique", shader::named(technique)),
        ("Subview", shader::named(subview)),
    ];

    let naming = Naming {
        resources: package
            .shaders()
            .iter()
            .flat_map(shpk::Shader::resources)
            .chain(package.constants())
            .chain(package.samplers())
            .chain(package.textures())
            .chain(package.uavs())
            .filter_map(|resource| Some((resource.id(), package.name(resource)?.to_owned())))
            .collect(),
        // The buffer a material fills, whose fields the reflection does not name; its registers are
        // read off the package's own parameter table instead.
        packed: Some(params::packed(
            names::hash(b"g_MaterialParameter"),
            &registers,
            &params,
        )),
        layouts,
    };

    Ok(Preview::Shpk(Box::new(Rendered {
        identity,
        resources,
        params,
        registers,
        defaults: package.param_defaults().to_vec(),
        keys: keys::read(&package),
        stages,
        shaders,
        naming,
        state: egui::Id::new(("shpk shader", path)),
    })))
}

pub fn ui(ui: &mut egui::Ui, package: &Rendered, bytes: &[u8]) {
    list::ui(ui, package, bytes);
}

/// One of the two readings a shader's text comes in, and what an exported file calls it.
#[derive(Clone, Copy)]
struct Reading {
    hlsl: bool,
    extension: &'static str,
    label: &'static str,
}

const HLSL: Reading = Reading {
    hlsl: true,
    extension: "hlsl",
    label: "HLSL",
};
const ASSEMBLY: Reading = Reading {
    hlsl: false,
    extension: "asm",
    label: "Assembly",
};

/// Beyond the raw file: every shader's chosen reading, zipped together where there is more than one
/// to write, and the pass the shader list's chips currently name as a merged source, if they name
/// exactly one of each. Reading `ctx`'s memory this way is one frame behind the chips themselves,
/// the same way `merged::reading` already is.
pub fn export_choices<'a>(
    package: &'a Rendered,
    bytes: &'a [u8],
    ctx: &egui::Context,
) -> Vec<export::Choice<'a>> {
    let mut choices = vec![
        shaders_choice(package, bytes, HLSL),
        shaders_choice(package, bytes, ASSEMBLY),
    ];
    if let Some(target) = merge_target(ctx, package) {
        choices.push(merged_choice(bytes, &target, HLSL));
        choices.push(merged_choice(bytes, &target, ASSEMBLY));
    }
    choices
}

fn shaders_choice<'a>(
    package: &'a Rendered,
    bytes: &'a [u8],
    reading: Reading,
) -> export::Choice<'a> {
    let single = package.shaders.len() == 1;
    let file_name = match single {
        true => format!("shader.{}", reading.extension),
        false => format!("shaders_{}.zip", reading.extension),
    };
    export::Choice::bytes(format!("All shaders, {}", reading.label), file_name, move || {
        let files: Vec<(String, Vec<u8>)> = package
            .shaders
            .iter()
            .enumerate()
            .filter_map(|(index, shader)| {
                let (lines, _) = shader::code::text(shader, &package.naming, bytes, reading.hlsl)?;
                Some((
                    format!("{index:04}_{}.{}", shader.stage, reading.extension),
                    lines.join("\n").into_bytes(),
                ))
            })
            .collect();
        match single {
            true => files
                .into_iter()
                .next()
                .map(|(_, data)| data)
                .ok_or_else(|| anyhow::anyhow!("no shader program in this package")),
            false => export::zip(&files),
        }
    })
}

/// The 0..4 stage index `shadermerge::pass` takes, its stage name, the pass id, and how many
/// shaders merging it would read.
struct MergeTarget {
    stage: usize,
    stage_name: &'static str,
    pass: u32,
    count: usize,
}

/// Where the shader list's two chip rows currently name exactly one stage and one pass with more
/// than one shader behind it. A single-shader "merge" is degenerate (the All-shaders choices
/// already cover it correctly) and `shadermerge`'s own struct synthesis can drop an implicit
/// output register in that case, so it is excluded here rather than offered and shown to fail.
fn merge_target(ctx: &egui::Context, package: &Rendered) -> Option<MergeTarget> {
    let (chip_stage, chip_pass, _) =
        ctx.data(|data| data.get_temp::<(usize, usize, usize)>(package.state))?;
    let count = list::mergeable(package, chip_stage, chip_pass).ok()?;
    if count < 2 {
        return None;
    }
    let (stage_name, ..) = package.stages.get(chip_stage.checked_sub(1)?)?;
    let stage = match *stage_name {
        "Vertex" => 0,
        "Pixel" => 1,
        "Hull" => 2,
        "Domain" => 3,
        "Geometry" => 4,
        _ => return None,
    };
    let pass = package.keys.passes.get(chip_pass.checked_sub(1)?)?.id;
    Some(MergeTarget {
        stage,
        stage_name,
        pass,
        count,
    })
}

fn merged_choice<'a>(
    bytes: &'a [u8],
    target: &MergeTarget,
    reading: Reading,
) -> export::Choice<'a> {
    let file_name = format!("merged_{}.{}", target.stage_name, reading.extension);
    let (stage, pass) = (target.stage, target.pass);
    export::Choice::bytes(format!("Merged pass, {}", reading.label), file_name, move || {
        let package = shpk::ShaderPackage::parse(bytes)?;
        let merged = shadermerge::pass(&package, bytes, stage, pass).map_err(anyhow::Error::from)?;
        let lines = match reading.hlsl {
            true => merged.lines,
            false => merged.asm,
        };
        Ok(lines.join("\n").into_bytes())
    })
    .hover(format!("{} shaders", target.count))
}

/// Everything about the package that is not a shader. It sits beside the code rather than above it,
/// where it would push the thing being read off the screen.
fn metadata_ui(ui: &mut egui::Ui, package: &Rendered) {
    if !package.registers.is_empty() {
        section(ui, "Material parameters");
        // Four columns of long names overflow a narrow panel, and only this table does.
        ScrollArea::horizontal()
            .id_salt("shpk_params_scroll")
            .show(ui, |ui| {
                // Or every name wraps to the width of a narrow panel instead of the table simply
                // being wider than one.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                params::ui(ui, &package.registers, &package.params, &package.defaults);
            });
        ui.add_space(8.0);
        ui.separator();
    }

    if package.resources.iter().any(|(_, rows)| !rows.is_empty()) {
        section(ui, "Resources");
        ScrollArea::horizontal()
            .id_salt("shpk_resources_scroll")
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                shader::resources_ui(ui, &package.resources);
            });
        ui.add_space(8.0);
        ui.separator();
    }

    if package.keys.any() {
        section(ui, "Keys");
        ScrollArea::horizontal()
            .id_salt("shpk_keys_scroll")
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                package.keys.ui(ui);
            });
    }
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            // Whichever shader the list has picked, so that clicking between two leaves this in
            // place and what differs is the rows that changed. A merged source is every shader of
            // the pass at once, so there is no one set of conditions it was compiled under.
            if let Some((_, _, picked)) = ui
                .data(|data| data.get_temp::<(usize, usize, usize)>(self.state))
                .filter(|_| !merged::reading(ui, self.state))
            {
                self.keys.defines_ui(ui, picked);
                ui.add_space(8.0);
                ui.separator();
            }
            facts(ui, "shpk_identity", &self.identity);
            ui.add_space(8.0);
            ui.separator();
            metadata_ui(ui, self);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use ironworks::sqpack::{Install, SqPack};

    fn createviewposition() -> (String, Vec<u8>) {
        let path = "shader/sm5/shpk/createviewposition.shpk".to_owned();
        let pack = SqPack::new(Install::at_sqpack("/home/asriel/.xlcore/ffxiv/game/sqpack"));
        let mut stream = pack.file(&path).expect("the shader is in the local install");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        (path, bytes)
    }

    /// The last stage/pass combination the chips could name whose shared shader count satisfies
    /// `matches`, so a test can ask for "more than one" or "exactly one" against a real corpus.
    fn find_combo(
        package: &super::Rendered,
        matches: impl Fn(usize) -> bool,
    ) -> Option<(usize, usize)> {
        let mut found = None;
        for stage_chip in 1..=package.stages.len() {
            for pass_chip in 1..=package.keys.passes.len() {
                if let Ok(count) = super::list::mergeable(package, stage_chip, pass_chip)
                    && matches(count)
                {
                    found = Some((stage_chip, pass_chip));
                }
            }
        }
        found
    }

    /// The chip state `list.rs` writes is a 1-based (stage row, pass row) pair; `merge_target`
    /// turns that back into the 0-based stage index `shadermerge::pass` takes and the pass's own
    /// id. Naming exactly one of each should add the Merged pair to the two All-shaders choices.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn export_choices_gain_the_merged_pair_once_the_chips_name_one_stage_and_one_pass() {
        let (path, bytes) = createviewposition();
        let preview = super::decode(&path, &bytes).expect("a real .shpk decodes");
        let super::Preview::Shpk(package) = preview else {
            panic!("decode() of a .shpk did not return Preview::Shpk");
        };
        let ctx = egui::Context::default();

        let plain = super::export_choices(&package, &bytes, &ctx);
        assert_eq!(plain.len(), 2, "no chip state set: only the two All-shaders choices");

        let (stage_chip, pass_chip) = find_combo(&package, |count| count >= 2)
            .expect("createviewposition.shpk has a stage and pass that share more than one shader");
        ctx.data_mut(|data| data.insert_temp(package.state, (stage_chip, pass_chip, 0usize)));

        let named = super::export_choices(&package, &bytes, &ctx);
        assert_eq!(
            named.len(),
            4,
            "one stage and one pass named: the Merged pair joins the All-shaders pair"
        );
    }

    /// The Pixel/`PASS_G_SEMITRANSPARENCY` pass in this package is exactly this case:
    /// `shadermerge::pass` emits `output.SV_Depth.x = ...` while its own synthesized `Output`
    /// struct declares only `SV_TARGET`, so `dxc` rejects it even though the same shader compiles
    /// clean through the plain per-shader export. The menu must never offer a single-shader merge,
    /// since the All-shaders choices already cover it and `merge_target` cannot tell this case from
    /// one `shadermerge` handles correctly.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn export_choices_omit_the_merged_pair_for_a_single_shader_target() {
        let (path, bytes) = createviewposition();
        let preview = super::decode(&path, &bytes).expect("a real .shpk decodes");
        let super::Preview::Shpk(package) = preview else {
            panic!("decode() of a .shpk did not return Preview::Shpk");
        };
        let ctx = egui::Context::default();

        let (stage_chip, pass_chip) = find_combo(&package, |count| count == 1)
            .expect("createviewposition.shpk has a stage and pass with exactly one shader");
        ctx.data_mut(|data| data.insert_temp(package.state, (stage_chip, pass_chip, 0usize)));

        let named = super::export_choices(&package, &bytes, &ctx);
        assert_eq!(
            named.len(),
            2,
            "a single-shader target stays at the two All-shaders choices, no Merged pair"
        );
    }

    /// A real small package, run manually against the local install and `dxc` (`cargo test -p
    /// viewer --lib -- --ignored shpk::tests --nocapture`, with `dxc` on `PATH`): the zip of every
    /// shader's HLSL, and the merged pass `shadermerge::pass` builds for it, both compile.
    #[test]
    #[ignore = "reads the real local FFXIV install and shells out to dxc"]
    fn the_export_producers_compile_under_dxc() {
        use ironworks::file::shpk;

        let (path, bytes) = createviewposition();
        let preview = super::decode(&path, &bytes).expect("a real .shpk decodes");
        let super::Preview::Shpk(package) = preview else {
            panic!("decode() of a .shpk did not return Preview::Shpk");
        };

        let target = |stage: &str| match stage {
            "Vertex" => "vs_6_0",
            "Pixel" => "ps_6_0",
            "Geometry" => "gs_6_0",
            "Hull" => "hs_6_0",
            "Domain" => "ds_6_0",
            other => panic!("no dxc target for stage {other}"),
        };
        let dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| ".".to_owned());
        let compile = |name: &str, source: &str, dxc_target: &str| {
            let out = std::path::Path::new(&dir).join(name);
            std::fs::write(&out, source).unwrap();
            let check = std::process::Command::new("dxc")
                .args(["-T", dxc_target, "-E", "main", "-Fo", "/dev/null"])
                .arg(&out)
                .output()
                .expect("dxc must be on PATH to run this check");
            assert!(
                check.status.success(),
                "dxc rejected {name}:\n{}",
                String::from_utf8_lossy(&check.stderr)
            );
        };

        let files: Vec<(String, Vec<u8>)> = package
            .shaders
            .iter()
            .enumerate()
            .filter_map(|(index, shader)| {
                let (lines, _) = super::shader::code::text(shader, &package.naming, &bytes, true)?;
                Some((format!("{index:04}_{}.hlsl", shader.stage), lines.join("\n").into_bytes()))
            })
            .collect();
        assert_eq!(files.len(), package.shaders.len(), "every shader decoded to HLSL");
        let zipped = super::export::zip(&files).expect("zipping the shader listing succeeds");
        println!("{} shaders zipped into {} bytes", files.len(), zipped.len());
        let (first_name, first_source) = &files[0];
        let first_target = target(&package.shaders[0].stage);
        compile(first_name, &String::from_utf8_lossy(first_source), first_target);

        let (stage_chip, pass_chip) = find_combo(&package, |count| count >= 2)
            .expect("createviewposition.shpk has a stage and pass that share more than one shader");
        let stage_name = package.stages[stage_chip - 1].0;
        let stage_index = match stage_name {
            "Vertex" => 0,
            "Pixel" => 1,
            "Hull" => 2,
            "Domain" => 3,
            "Geometry" => 4,
            other => panic!("no shadermerge stage index for {other}"),
        };
        let pass_id = package.keys.passes[pass_chip - 1].id;

        let raw = shpk::ShaderPackage::parse(&bytes).expect("the raw package parses");
        let merged = shadermerge::pass(&raw, &bytes, stage_index, pass_id)
            .unwrap_or_else(|error| panic!("shadermerge::pass failed: {error:?}"));
        println!("merged {stage_name} pass into {} lines", merged.lines.len());
        compile("shpk_export_check_merged.hlsl", &merged.lines.join("\n"), target(stage_name));
    }
}
