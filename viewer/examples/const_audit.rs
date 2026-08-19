//! Which constant buffer fields the packages the viewer runs read, per package.
//!
//! `const_audit [extra paths]`

use std::collections::{BTreeMap, BTreeSet};

use dxbc::chunks::ChunkData;
use dxbc::shex::{ComponentSelect, InstructionKind, Opcode, OperandIndex, RegisterType};
use ironworks::{
    Ironworks,
    file::{
        shcd,
        shpk::{self, ShaderPackage, Stage},
    },
    sqpack::{Install, IndexHash, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;
const SUB_VIEW_SHADOW_0: u32 = 0x99b2_2d1c;

const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const PASS_G_SEMITRANSPARENCY: u32 = 0x6006_067f;
const PASS_Z_OPAQUE: u32 = 0xe412_a2d4;
const PASS_LIGHTING_OPAQUE: u32 = 0xfbde_0a8f;
const PASS_COMPOSITE_OPAQUE: u32 = 0x955c_0b73;
const PASS_COMPOSITE_SEMITRANSPARENCY: u32 = 0xc885_bbd3;
const PASS_LIGHTING_SEMITRANSPARENCY: u32 = 0x1f19_7698;
const PASS_WATER: u32 = 0x8ef4_0d56;
const PASS_7: u32 = 0x5bc1_ad3f;

const GET_NORMAL_MAP: (u32, u32) = (0xcbdf_d5ec, 0xd999_4ef1);
const APPLY_ALPHA_CLIP: (u32, u32) = (0xdcfc_844e, 0x59c4_e6db);
const APPLY_DETAIL_MAP: (u32, u32) = (0x6313_fd87, 0x7a3d_9efd);
const APPLY_WAVING_ANIM: (u32, u32) = (0x105c_6a52, 0xf801_b859);
const TRANSFORM_VIEW: (u32, u32) = (0xa5a1_910d, 0x9c14_c8e9);
const GET_DIRECTIONAL_LIGHT: (u32, u32) = (0x8115_916d, 0xd73b_9e89);
const SPECULAR_LIGHTING: (u32, u32) = (0x0d81_2fa4, 0xaba1_f498);
const SHADOW_SOFT: (u32, u32) = (0xa89d_89f0, 0x9915_3ff0);

const CLOUD_BAND: u32 = 0xa2f7_6b97;
const CLOUD_SHEET: u32 = 0xd9d5_8038;

fn selector(keys: &[u32]) -> u32 {
    let (mut out, mut mul) = (0u32, 1u32);
    for key in keys {
        out = out.wrapping_add(key.wrapping_mul(mul));
        mul = mul.wrapping_mul(31);
    }
    out
}

fn values(keys: &[shpk::Key], set: &[(u32, u32)]) -> Vec<u32> {
    keys.iter()
        .map(|key| {
            set.iter()
                .find(|(id, _)| *id == key.id())
                .map(|(_, value)| *value)
                .unwrap_or_else(|| key.default_value())
        })
        .collect()
}

/// The pair `program.rs::pair` would take, by the same lookup.
fn pair(
    package: &ShaderPackage,
    set: &[(u32, u32)],
    pass: u32,
    technique: u32,
    subview: u32,
) -> Option<(u32, u32)> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, set)))
    .collect();
    parts.push(selector(&[technique, subview]));
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

/// The shaders the viewer's own selection reaches, for the packages it binds.
fn selection(path: &str, package: &ShaderPackage) -> BTreeSet<u32> {
    let technique = package.technique_subview()[0];
    let screen: Vec<(u32, u32)> = vec![GET_DIRECTIONAL_LIGHT, SPECULAR_LIGHTING];
    let mut out = BTreeSet::new();
    let mut take = |set: &[(u32, u32)], pass, technique, subview| {
        if let Some((vs, ps)) = pair(package, set, pass, technique, subview) {
            out.insert(vs);
            out.insert(ps);
        }
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "createviewposition.shpk" | "directionallighting.shpk" | "subsurfaceblur.shpk" => {
            take(&screen, PASS_LIGHTING_OPAQUE, technique, SUB_VIEW_MAIN);
        }
        "pointlighting.shpk" | "spotlighting.shpk" | "linelighting.shpk"
        | "planelighting.shpk" => {
            take(&screen, PASS_LIGHTING_OPAQUE, technique, SUB_VIEW_MAIN);
        }
        "directionalshadow.shpk" => {
            let mut set = screen.clone();
            set.push(SHADOW_SOFT);
            take(&set, PASS_LIGHTING_OPAQUE, technique, SUB_VIEW_MAIN);
        }
        "bg_composite.shpk" => take(&screen, PASS_COMPOSITE_OPAQUE, technique, SUB_VIEW_MAIN),
        "furblur.shpk" => take(&screen, PASS_7, technique, SUB_VIEW_MAIN),
        "cloud.shpk" => {
            let subview = package.technique_subview()[1];
            take(&[], PASS_7, CLOUD_BAND, subview);
            take(&[], PASS_7, CLOUD_SHEET, subview);
        }
        _ => {
            let zone = vec![GET_NORMAL_MAP, APPLY_ALPHA_CLIP, APPLY_DETAIL_MAP];
            let model = vec![GET_NORMAL_MAP, APPLY_ALPHA_CLIP];
            let sets = [
                zone.clone(),
                [zone.clone(), vec![APPLY_WAVING_ANIM]].concat(),
                model.clone(),
                [model.clone(), vec![TRANSFORM_VIEW]].concat(),
                [model.clone(), vec![APPLY_WAVING_ANIM]].concat(),
                [model, vec![TRANSFORM_VIEW, APPLY_WAVING_ANIM]].concat(),
            ];
            for set in &sets {
                for pass in [
                    PASS_Z_OPAQUE,
                    PASS_G_OPAQUE,
                    PASS_G_SEMITRANSPARENCY,
                    PASS_COMPOSITE_OPAQUE,
                    PASS_COMPOSITE_SEMITRANSPARENCY,
                    PASS_WATER,
                    PASS_LIGHTING_SEMITRANSPARENCY,
                ] {
                    take(set, pass, technique, SUB_VIEW_MAIN);
                }
                take(set, PASS_Z_OPAQUE, technique, SUB_VIEW_SHADOW_0);
            }
        }
    }
    out
}

const PACKAGES: [&str; 26] = [
    "shader/sm5/shpk/createviewposition.shpk",
    "shader/sm5/shpk/directionallighting.shpk",
    "shader/sm5/shpk/pointlighting.shpk",
    "shader/sm5/shpk/spotlighting.shpk",
    "shader/sm5/shpk/linelighting.shpk",
    "shader/sm5/shpk/planelighting.shpk",
    "shader/sm5/shpk/directionalshadow.shpk",
    "shader/sm5/shpk/bg_composite.shpk",
    "shader/sm5/shpk/furblur.shpk",
    "shader/sm5/shpk/subsurfaceblur.shpk",
    "shader/sm5/shpk/cloud.shpk",
    "shader/sm5/shpk/bg.shpk",
    "shader/sm5/shpk/bgcolorchange.shpk",
    "shader/sm5/shpk/bgprop.shpk",
    "shader/sm5/shpk/crystal.shpk",
    "shader/sm5/shpk/water.shpk",
    "shader/sm5/shpk/river.shpk",
    "shader/sm5/shpk/grass.shpk",
    "shader/sm5/shpk/lightshaft.shpk",
    "shader/sm5/shpk/verticalfog.shpk",
    "shader/sm5/shpk/character.shpk",
    "shader/sm5/shpk/characterglass.shpk",
    "shader/sm5/shpk/characterlegacy.shpk",
    "shader/sm5/shpk/hair.shpk",
    "shader/sm5/shpk/iris.shpk",
    "shader/sm5/shpk/skin.shpk",
];

const SHCDS: [&str; 22] = [
    "shader/sm5/posteffect/ToneAdjust.shcd",
    "shader/sm5/posteffect/FXAALuma.shcd",
    "shader/sm5/posteffect/FXAA.shcd",
    "shader/sm5/posteffect/MeasureLumInitial.shcd",
    "shader/sm5/posteffect/MeasureLumIterative.shcd",
    "shader/sm5/posteffect/MeasureLumFinal.shcd",
    "shader/sm5/posteffect/AdaptLum.shcd",
    "shader/sm5/posteffect/ToneMapLut.shcd",
    "shader/sm5/posteffect/ToneMapping.shcd",
    "shader/sm5/posteffect/Sky.shcd",
    "shader/sm5/posteffect/Sun.shcd",
    "shader/sm5/posteffect/Moon.shcd",
    "shader/sm5/posteffect/DownScaleDepthNormalZ.shcd",
    "shader/sm5/posteffect/GatherDepthNormalZ.shcd",
    "shader/sm5/posteffect/SSAO1.shcd",
    "shader/sm5/posteffect/SSAO2.shcd",
    "shader/sm5/posteffect/SSAO3.shcd",
    "shader/sm5/posteffect/SSAO4.shcd",
    "shader/sm5/posteffect/SSAO5.shcd",
    "shader/sm5/posteffect/SSAO6.shcd",
    "shader/sm5/posteffect/SSAO7.shcd",
    "shader/sm5/posteffect/SSAO8.shcd",
];

/// What one shader was seen to touch of one buffer.
#[derive(Default)]
struct Touched {
    /// Byte offsets read, at component granularity.
    offsets: BTreeSet<u32>,
    /// Registers read where the index was worked out at runtime.
    dynamic: bool,
}

#[derive(Default)]
struct Held {
    /// Shaders declaring the buffer, and shaders reading each member.
    declaring: u32,
    reading: BTreeMap<String, u32>,
    unread: BTreeMap<String, u32>,
    registers: BTreeMap<u32, u32>,
    dynamic: u32,
    members: Vec<(String, u32, u32, String)>,
    size: u32,
}

/// Whether a source operand's swizzle is cut down by where the instruction writes.
fn componentwise(opcode: Opcode) -> bool {
    !matches!(
        opcode,
        Opcode::Dp2
            | Opcode::Dp3
            | Opcode::Dp4
            | Opcode::Sample
            | Opcode::SampleB
            | Opcode::SampleC
            | Opcode::SampleCLz
            | Opcode::SampleD
            | Opcode::SampleL
            | Opcode::Gather4
            | Opcode::Ld
            | Opcode::LdMs
            | Opcode::Resinfo
            | Opcode::Sincos
            | Opcode::IMul
            | Opcode::UMul
            | Opcode::UDiv
            | Opcode::Discard
            | Opcode::If
            | Opcode::Breakc
            | Opcode::Continuec
            | Opcode::Retc
            | Opcode::Switch
    )
}

fn walk(blob: &[u8], into: &mut BTreeMap<String, Held>) {
    let chunks: Vec<_> = dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .map(|chunk| chunk.parse())
        .collect();
    let Some(ChunkData::Rdef(rdef)) = chunks.iter().find(|held| matches!(held, ChunkData::Rdef(_)))
    else {
        return;
    };
    let Some(ChunkData::Shader(program)) = chunks
        .iter()
        .find(|held| matches!(held, ChunkData::Shader(_)))
    else {
        return;
    };
    // Which buffer each `cb#` slot names, and what its reflection says it holds.
    let mut slots: BTreeMap<u32, &str> = BTreeMap::new();
    for binding in &rdef.bindings {
        if binding.input_type == 0 {
            slots.insert(binding.bind_point, binding.name.as_ref());
        }
    }
    let mut touched: BTreeMap<&str, Touched> = BTreeMap::new();
    for instruction in &program.instructions {
        if matches!(instruction.kind, InstructionKind::DclConstantBuffer { .. }) {
            continue;
        }
        let operands = instruction.operands();
        let mask = match operands.first().map(|held| &held.components) {
            Some(ComponentSelect::Mask(bits)) if componentwise(instruction.opcode) => Some(*bits),
            _ => None,
        };
        for operand in operands.iter().skip(1) {
            collect(operand, mask, &slots, &mut touched);
            // A relative index is itself an operand, and it may be a buffer read of its own.
            for index in &operand.indices {
                let (OperandIndex::Relative(inner) | OperandIndex::RelativePlusImm(_, inner)) =
                    index
                else {
                    continue;
                };
                collect(inner, None, &slots, &mut touched);
            }
        }
    }
    for buffer in &rdef.constant_buffers {
        let members = hlsl::layout::members(buffer);
        let held = into.entry(buffer.name.to_string()).or_default();
        held.declaring += 1;
        held.size = held.size.max(buffer.size);
        if held.members.is_empty() {
            held.members = members
                .iter()
                .map(|member| {
                    (
                        member.name.clone(),
                        member.offset,
                        member.size,
                        member.kind.clone(),
                    )
                })
                .collect();
        }
        let Some(seen) = touched.get(buffer.name.as_ref()) else {
            continue;
        };
        if seen.dynamic {
            held.dynamic += 1;
        }
        for offset in &seen.offsets {
            *held.registers.entry(offset / 16).or_default() += 1;
        }
        let hit: BTreeSet<&str> = members
            .iter()
            .filter(|member| {
                seen.offsets
                    .iter()
                    .any(|at| *at >= member.offset && *at < member.offset + member.size)
            })
            .map(|member| member.name.as_str())
            .collect();
        for member in &members {
            let into = match hit.contains(member.name.as_str()) {
                true => &mut held.reading,
                false => &mut held.unread,
            };
            *into.entry(member.name.clone()).or_default() += 1;
        }
    }
}

fn collect<'a>(
    operand: &dxbc::shex::Operand,
    mask: Option<u8>,
    slots: &BTreeMap<u32, &'a str>,
    into: &mut BTreeMap<&'a str, Touched>,
) {
    if operand.reg_type != RegisterType::ConstantBuffer {
        return;
    }
    let Some(OperandIndex::Imm32(slot)) = operand.indices.first() else {
        return;
    };
    let Some(name) = slots.get(slot) else {
        return;
    };
    let held = into.entry(name).or_default();
    let Some(index) = operand.indices.get(1) else {
        return;
    };
    let register = match index {
        OperandIndex::Imm32(at) => *at,
        _ => {
            held.dynamic = true;
            return;
        }
    };
    let lanes: Vec<u8> = match &operand.components {
        ComponentSelect::Scalar(at) => vec![*at],
        ComponentSelect::Swizzle(held) => match mask {
            Some(bits) => (0..4).filter(|at| bits & (1 << at) != 0).map(|at| held[at as usize]).collect(),
            None => held.to_vec(),
        },
        ComponentSelect::Mask(bits) => (0..4).filter(|at| bits & (1 << at) != 0).collect(),
        _ => vec![0],
    };
    for lane in lanes {
        held.offsets.insert(register * 16 + u32::from(lane) * 4);
    }
}

fn report(path: &str, shaders: usize, held: &BTreeMap<String, Held>) {
    println!("\n== {path} ({shaders} shaders)");
    for (name, buffer) in held {
        let bare = buffer.members.is_empty();
        println!(
            "  BUFFER {name} size {} declared by {} {}{}",
            buffer.size,
            buffer.declaring,
            match bare {
                true => "BARE-ARRAY",
                false => "",
            },
            match buffer.dynamic {
                0 => String::new(),
                count => format!(" dynamic-index in {count}"),
            }
        );
        for (member, offset, size, kind) in &buffer.members {
            let read = buffer.reading.get(member).copied().unwrap_or(0);
            println!(
                "    {:<40} +{:<5} {:>3}B reg {:<3} {:<12} read by {}/{}",
                member,
                offset,
                size,
                offset / 16,
                kind,
                read,
                buffer.declaring
            );
        }
        if bare {
            let registers: Vec<String> = buffer
                .registers
                .iter()
                .map(|(at, count)| format!("{at}({count})"))
                .collect();
            println!("    registers read: {}", registers.join(" "));
        }
    }
}

fn main() {
    let ironworks =
        Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK.to_owned())));
    let sqpack = SqPack::new(Install::at_sqpack(SQPACK.to_owned()));

    let extra: Vec<String> = std::env::args().skip(1).collect();
    for path in PACKAGES.iter().map(|held| (*held).to_owned()).chain(extra) {
        let Ok(raw) = ironworks.file::<Vec<u8>>(&path) else {
            println!("\n== {path} MISSING");
            continue;
        };
        let Ok(package) = ShaderPackage::parse(&raw) else {
            println!("\n== {path} UNPARSED");
            continue;
        };
        let taken = selection(&path, &package);
        let mut held = BTreeMap::new();
        for index in &taken {
            let Some(shader) = package.shaders().get(*index as usize) else {
                continue;
            };
            let start = package.blobs_offset() + shader.blob_offset() as usize;
            let blob = &raw[start..start + shader.blob_size() as usize];
            walk(blob, &mut held);
        }
        report(&path, taken.len(), &held);
        println!("  SHADERS {taken:?}");
    }

    let fog = {
        let (Some(IndexHash::Split(directory)), _) = IndexHash::of("shader/sm5/posteffect/x")
        else {
            unreachable!()
        };
        IndexHash::Split(directory & !0xffff_ffff | 0xe8bf_3721)
    };
    let mut files: Vec<(String, Vec<u8>)> = SHCDS
        .iter()
        .filter_map(|path| {
            ironworks
                .file::<Vec<u8>>(path)
                .ok()
                .map(|raw| ((*path).to_owned(), raw))
        })
        .collect();
    match sqpack.file_by_hash(0, 5, fog) {
        Ok(mut file) => {
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut raw).expect("fog bytes");
            files.push(("shader/sm5/posteffect/e8bf3721".to_owned(), raw));
        }
        Err(why) => println!("\n== shader/sm5/posteffect/e8bf3721 MISSING: {why}"),
    }
    for (path, raw) in &files {
        let Ok(code) = shcd::ShaderCode::parse(raw) else {
            println!("\n== {path} UNPARSED");
            continue;
        };
        let blob = &raw[code.blob_offset()..code.blob_offset() + code.blob_size()];
        let mut held = BTreeMap::new();
        walk(blob, &mut held);
        report(path, 1, &held);
    }
}
