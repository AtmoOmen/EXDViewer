//! Where the two stages of a pair the viewer translates declare one constant buffer at different
//! extents, and what a fill to the vertex stage's own extent left past the end.
//!
//! `extent_sweep [package ...] [--materials <path list>]`

use std::collections::{BTreeMap, BTreeSet};

use dxbc::chunks::ChunkData;
use dxbc::shex::{InstructionKind, OperandIndex};
use ironworks::{
    Ironworks,
    file::{
        mtrl, shcd,
        shpk::{self, ShaderPackage, Stage},
    },
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// Every pass `Pass::id` names.
const PASSES: [(&str, u32); 12] = [
    ("Sprite@", 0xc5a5_389c),
    ("Z@", 0xe412_a2d4),
    ("G@", 0x03ac_862e),
    ("GSemi@", 0x6006_067f),
    ("Light@", 0xfbde_0a8f),
    ("Pass7@", 0x5bc1_ad3f),
    ("Comp@", 0x955c_0b73),
    ("CompSemi@", 0xc885_bbd3),
    ("LightSemi@", 0x1f19_7698),
    ("Water@", 0x8ef4_0d56),
    ("Semi@", 0x2d0c_1a37),
    ("WaterZ@", 0x24cd_f1ea),
];

const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;
const SUB_VIEW_SHADOW_0: u32 = 0x99b2_2d1c;
const MAIN: u32 = 0xa8f9_ffcc;

/// The keys the two viewers set on every draw of their own, rather than reading off the material.
const ALPHA_CLIP: (u32, u32) = (0xdcfc_844e, 0x59c4_e6db);
const RLR: (u32, u32) = (0x1143_3f2d, 0x4ba7_7904);
const NORMAL_MAP: (u32, u32) = (0xcbdf_d5ec, 0xd999_4ef1);
const NORMAL_MAP_PARALLAX: (u32, u32) = (0xcbdf_d5ec, 0xd9fd_8a1c);
const SKINNED: (u32, u32) = (0xa5a1_910d, 0x9c14_c8e9);
const WAVING: (u32, u32) = (0x105c_6a52, 0xf801_b859);

/// Everything the viewer can bind: the packages a material may name, the ones the engine runs over
/// the frame, and the two apricot draws with.
const LIST: &str = include_str!("../../smoke/shpk_names.txt");
const EXTRA: [&str; 6] = [
    "shader/sm5/shpk/linelighting.shpk",
    "shader/sm5/shpk/planelighting.shpk",
    "shader/sm5/shpk/directionalshadow.shpk",
    "shader/sm5/shpk/subsurfaceblur.shpk",
    "shader/sm5/shpk/apricot_shape.shpk",
    "shader/sm5/shpk/apricot_model.shpk",
];

const STARS: [&str; 2] = [
    "shader/sm5/shcd/starvs0_gu.shcd",
    "shader/sm5/shcd/starps0_gu.shcd",
];

/// What one stage says about its buffers: how many registers it declares each at, and what its own
/// reflection describes each as.
#[derive(Default)]
struct Stated {
    extents: BTreeMap<String, u32>,
    members: BTreeMap<String, Vec<(String, u32, u32)>>,
}

impl Stated {
    /// Bytes the reflection's own fields reach, which is what `Buffer::fill` sizes against.
    fn span(&self, name: &str) -> u32 {
        self.members
            .get(name)
            .map(|held| {
                held.iter()
                    .map(|(_, offset, size)| offset + size)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }
}

fn stated(blob: &[u8], named: &dyn Fn(u16) -> String) -> Stated {
    let mut out = Stated::default();
    let chunks: Vec<_> = dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .map(|chunk| chunk.parse())
        .collect();
    if let Some(ChunkData::Rdef(rdef)) = chunks.iter().find(|held| matches!(held, ChunkData::Rdef(_)))
    {
        for buffer in &rdef.constant_buffers {
            out.members.insert(
                buffer.name.to_string(),
                hlsl::layout::members(buffer)
                    .into_iter()
                    .map(|member| (member.name, member.offset, member.size))
                    .collect(),
            );
        }
    }
    let Some(ChunkData::Shader(program)) = chunks
        .iter()
        .find(|held| matches!(held, ChunkData::Shader(_)))
    else {
        return out;
    };
    for instruction in &program.instructions {
        if !matches!(instruction.kind, InstructionKind::DclConstantBuffer { .. }) {
            continue;
        }
        let Some(operand) = instruction.operands().first() else {
            continue;
        };
        let (Some(OperandIndex::Imm32(slot)), Some(OperandIndex::Imm32(span))) =
            (operand.indices.first(), operand.indices.get(1))
        else {
            continue;
        };
        let held = out.extents.entry(named(*slot as u16)).or_insert(0);
        *held = (*held).max(*span);
    }
    out
}

/// Bytes `Buffer::fill` lays out for a buffer the reflection describes to `span`, held at
/// `registers`.
fn filled(span: u32, registers: u32) -> u32 {
    span.max(registers * 16).max(16).div_ceil(16) * 16
}

/// One buffer whose two stages describe their fields differently, which `layouts` settles by
/// taking the vertex stage's.
struct Described {
    vs: Vec<(String, u32, u32)>,
    ps: Vec<(String, u32, u32)>,
    pairs: usize,
}

/// One buffer two stages disagree about, and every pair the disagreement was reached through.
#[derive(Default)]
struct Split {
    pairs: BTreeSet<(u32, u32)>,
    passes: BTreeSet<&'static str>,
    vs_members: Vec<(String, u32, u32)>,
    ps_members: Vec<(String, u32, u32)>,
    vs_span: u32,
    ps_span: u32,
}

fn report(
    path: &str,
    nodes: usize,
    pairs: usize,
    split: &BTreeMap<(String, u32, u32), Split>,
    described: &BTreeMap<String, Described>,
) {
    println!("\n== {path}  nodes {nodes} pairs {pairs}");
    for (name, held) in described {
        let vs: BTreeSet<&str> = held.vs.iter().map(|(name, ..)| name.as_str()).collect();
        let ps: BTreeSet<&str> = held.ps.iter().map(|(name, ..)| name.as_str()).collect();
        println!(
            "  DESCRIBED {name:<28} vs {} ps {} pairs {}  only in ps {:?}  only in vs {:?}",
            held.vs.len(),
            held.ps.len(),
            held.pairs,
            ps.difference(&vs).collect::<Vec<_>>(),
            vs.difference(&ps).collect::<Vec<_>>(),
        );
    }
    if split.is_empty() {
        println!("  none");
        return;
    }
    for ((name, vs, ps), held) in split {
        let filled = filled(held.vs_span, *vs);
        let declared = (*vs).max(*ps) * 16;
        let verdict = match filled < declared {
            true => format!("ZEROED {filled}..{declared}"),
            false => "covered".to_owned(),
        };
        println!(
            "  {name:<32} vs {vs:>3} ps {ps:>3}  members vs {}/{} ps {}/{}  pairs {}  {verdict}",
            held.vs_members.len(),
            held.vs_span,
            held.ps_members.len(),
            held.ps_span,
            held.pairs.len(),
        );
        println!("    passes {:?}", held.passes);
        let vs_names: BTreeSet<&str> = held.vs_members.iter().map(|(name, ..)| name.as_str()).collect();
        let lost: Vec<&str> = held
            .ps_members
            .iter()
            .map(|(name, ..)| name.as_str())
            .filter(|name| !vs_names.contains(name))
            .collect();
        if !lost.is_empty() && !held.vs_members.is_empty() {
            println!("    fields only the pixel stage describes: {lost:?}");
        }
    }
}

/// A package's splits, by the pair each was reached through, so a material's own selection can be
/// looked up against them.
type Splits = BTreeMap<(u32, u32), Vec<(String, u32, u32, u32, u32)>>;

fn sweep(path: &str, raw: &[u8]) -> Option<(ShaderPackage, Splits)> {
    let Ok(package) = ShaderPackage::parse(raw) else {
        println!("\n== {path} UNPARSED");
        return None;
    };
    let blob = |index: u32| -> Option<&[u8]> {
        let shader = package.shaders().get(index as usize)?;
        let start = package.blobs_offset() + shader.blob_offset() as usize;
        raw.get(start..start + shader.blob_size() as usize)
    };
    let named = |index: u32| {
        let package = &package;
        move |slot: u16| {
            package
                .shaders()
                .get(index as usize)
                .and_then(|shader| {
                    shader
                        .constants()
                        .iter()
                        .find(|held| held.slot() == slot)
                        .and_then(|held| package.name(held))
                })
                .map(str::to_owned)
                .unwrap_or_else(|| format!("cb{slot}"))
        }
    };
    let mut read: BTreeMap<u32, Stated> = BTreeMap::new();
    let state = |index: u32, read: &mut BTreeMap<u32, Stated>| {
        if !read.contains_key(&index)
            && let Some(bytes) = blob(index)
        {
            read.insert(index, stated(bytes, &named(index)));
        }
    };
    let base = |want: Stage| {
        package
            .shaders()
            .iter()
            .take_while(|shader| shader.stage() != want)
            .count() as u32
    };
    let (vertices, pixels) = (base(Stage::Vertex), base(Stage::Pixel));

    let mut split: BTreeMap<(String, u32, u32), Split> = BTreeMap::new();
    let mut described: BTreeMap<String, Described> = BTreeMap::new();
    let mut pairs: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut splits: Splits = Splits::new();
    for node in package.nodes() {
        for (label, id) in PASSES {
            let Some(held) = node.passes().iter().find(|held| held.id() == id) else {
                continue;
            };
            if held.vertex() == shpk::NONE || held.pixel() == shpk::NONE {
                continue;
            }
            let (vs, ps) = (vertices + held.vertex(), pixels + held.pixel());
            pairs.insert((vs, ps));
            state(vs, &mut read);
            state(ps, &mut read);
            let (Some(vertex), Some(fragment)) = (read.get(&vs), read.get(&ps)) else {
                continue;
            };
            for name in vertex.members.keys() {
                let (Some(one), Some(other)) =
                    (vertex.members.get(name), fragment.members.get(name))
                else {
                    continue;
                };
                if one == other {
                    continue;
                }
                let entry = described.entry(name.clone()).or_insert_with(|| Described {
                    vs: one.clone(),
                    ps: other.clone(),
                    pairs: 0,
                });
                entry.pairs += 1;
            }
            for (name, count) in &vertex.extents {
                let Some(other) = fragment.extents.get(name) else {
                    continue;
                };
                if other == count {
                    continue;
                }
                let entry = split
                    .entry((name.clone(), *count, *other))
                    .or_insert_with(|| Split {
                        vs_members: vertex.members.get(name).cloned().unwrap_or_default(),
                        ps_members: fragment.members.get(name).cloned().unwrap_or_default(),
                        vs_span: vertex.span(name),
                        ps_span: fragment.span(name),
                        ..Split::default()
                    });
                entry.pairs.insert((vs, ps));
                entry.passes.insert(label);
                splits.entry((vs, ps)).or_default().push((
                    name.clone(),
                    *count,
                    *other,
                    filled(entry.vs_span, *count),
                    (*count).max(*other) * 16,
                ));
            }
        }
    }
    report(path, package.nodes().len(), pairs.len(), &split, &described);
    Some((package, splits))
}

fn selector(keys: &[u32]) -> u32 {
    let (mut out, mut mul) = (0u32, 1u32);
    for key in keys {
        out = out.wrapping_add(key.wrapping_mul(mul));
        mul = mul.wrapping_mul(31);
    }
    out
}

/// The pair `program.rs::picks` would take, by the same lookup and the same fallback.
fn picks(
    package: &ShaderPackage,
    material: &[mtrl::ShaderKey],
    set: &[(u32, u32)],
    pass: u32,
    subview: u32,
) -> Option<(u32, u32)> {
    let held = |subview| {
        let mut parts: Vec<u32> = [
            package.system_keys(),
            package.scene_keys(),
            package.material_keys(),
        ]
        .iter()
        .map(|keys| {
            selector(
                &keys
                    .iter()
                    .map(|key| {
                        set.iter()
                            .find(|(id, _)| *id == key.id())
                            .map(|(_, value)| *value)
                            .or_else(|| {
                                material
                                    .iter()
                                    .find(|one| one.category() == key.id())
                                    .map(mtrl::ShaderKey::value)
                            })
                            .unwrap_or_else(|| key.default_value())
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
        parts.push(selector(&[package.technique_subview()[0], subview]));
        let id = selector(&parts);
        let node = package
            .nodes()
            .iter()
            .find(|node| node.id() == id)
            .or_else(|| {
                let alias = package.aliases().iter().find(|held| held.selector() == id)?;
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
    };
    held(subview).or_else(|| (subview == SUB_VIEW_MAIN).then(|| held(MAIN)).flatten())
}

/// Every draw the two viewers make of a material's own package.
const DRAWS: [(u32, u32); 10] = [
    (0xe412_a2d4, SUB_VIEW_MAIN),
    (0x955c_0b73, SUB_VIEW_MAIN),
    (0xe412_a2d4, SUB_VIEW_SHADOW_0),
    (0x03ac_862e, SUB_VIEW_MAIN),
    (0x6006_067f, SUB_VIEW_MAIN),
    (0xc885_bbd3, SUB_VIEW_MAIN),
    (0x8ef4_0d56, SUB_VIEW_MAIN),
    (0x1f19_7698, SUB_VIEW_MAIN),
    (0x2d0c_1a37, SUB_VIEW_MAIN),
    (0x24cd_f1ea, SUB_VIEW_MAIN),
];

/// The engine key sets a material's package is reached under: skinned or not, waving or not.
fn engine_sets(name: &str) -> Vec<Vec<(u32, u32)>> {
    let normal = match name.ends_with("/bg.shpk") {
        true => NORMAL_MAP_PARALLAX,
        false => NORMAL_MAP,
    };
    let mut out = Vec::new();
    for skinned in [false, true] {
        for waving in [false, true] {
            let mut held = vec![ALPHA_CLIP, RLR, normal];
            if skinned {
                held.push(SKINNED);
            }
            if waving {
                held.push(WAVING);
            }
            out.push(held);
        }
    }
    out
}

/// Which of a package's splits a real material's own keys reach, over every `.mtrl` the game ships.
fn narrow(
    ironworks: &Ironworks<SqPack<Install>>,
    held: &BTreeMap<String, (ShaderPackage, Splits)>,
    list: &str,
) {
    let mut seen: BTreeSet<(String, Vec<(u32, u32)>)> = BTreeSet::new();
    let mut materials: BTreeMap<String, usize> = BTreeMap::new();
    let mut reached: BTreeMap<(String, String, u32, u32), (usize, u32, u32, String)> =
        BTreeMap::new();
    let mut read = 0usize;
    for path in std::fs::read_to_string(list).expect("the path list").lines() {
        if !path.ends_with(".mtrl") {
            continue;
        }
        let Ok(material) = ironworks.file::<mtrl::Material>(path) else {
            continue;
        };
        read += 1;
        let name = format!("shader/sm5/shpk/{}", material.shader());
        let Some((package, splits)) = held.get(&name) else {
            continue;
        };
        *materials.entry(name.clone()).or_default() += 1;
        let keys: Vec<(u32, u32)> = material
            .shader_keys()
            .iter()
            .map(|key| (key.category(), key.value()))
            .collect();
        if !seen.insert((name.clone(), keys)) {
            continue;
        }
        for set in engine_sets(&name) {
            for (pass, subview) in DRAWS {
                let Some(pair) = picks(package, material.shader_keys(), &set, pass, subview) else {
                    continue;
                };
                for (buffer, vs, ps, filled, declared) in splits.get(&pair).into_iter().flatten() {
                    let entry = reached
                        .entry((name.clone(), buffer.clone(), *vs, *ps))
                        .or_insert((0, *filled, *declared, path.to_owned()));
                    entry.0 += 1;
                }
            }
        }
    }
    println!("\n== materials read {read}");
    for (name, count) in &materials {
        println!("  {name} {count}");
    }
    for ((name, buffer, vs, ps), (count, filled, declared, one)) in &reached {
        let verdict = match filled < declared {
            true => format!("ZEROED {filled}..{declared}"),
            false => "covered".to_owned(),
        };
        println!("  REACHED {name} {buffer} vs {vs} ps {ps} draws {count}  {verdict}  {one}");
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let extra: Vec<String> = std::env::args().skip(1).collect();
    let list = extra.iter().position(|held| held == "--materials");
    let paths = list.and_then(|at| extra.get(at + 1)).cloned();
    let extra: Vec<String> = match list {
        Some(at) => extra[..at].to_vec(),
        None => extra,
    };
    let wanted: Vec<String> = match extra.is_empty() {
        true => LIST
            .split_whitespace()
            .chain(EXTRA)
            .map(ToOwned::to_owned)
            .collect(),
        false => extra,
    };
    let mut held: BTreeMap<String, (ShaderPackage, Splits)> = BTreeMap::new();
    for path in wanted {
        let Ok(raw) = ironworks.file::<Vec<u8>>(&path) else {
            continue;
        };
        if let Some(swept) = sweep(&path, &raw) {
            held.insert(path, swept);
        }
    }
    if let Some(paths) = paths {
        narrow(&ironworks, &held, &paths);
    }

    let read = |path: &str| -> Option<Stated> {
        let raw = ironworks.file::<Vec<u8>>(path).ok()?;
        let code = shcd::ShaderCode::parse(&raw).ok()?;
        let blob = raw.get(code.blob_offset()..code.blob_offset() + code.blob_size())?;
        let slots: BTreeMap<u16, String> = code
            .constants()
            .iter()
            .filter_map(|resource| Some((resource.slot(), code.name(resource)?.to_owned())))
            .collect();
        Some(stated(blob, &|slot| {
            slots.get(&slot).cloned().unwrap_or_else(|| format!("cb{slot}"))
        }))
    };
    let [vertex, fragment] = STARS;
    let (Some(vertex), Some(fragment)) = (read(vertex), read(fragment)) else {
        println!("\n== stars MISSING");
        return;
    };
    let mut split: BTreeMap<(String, u32, u32), Split> = BTreeMap::new();
    for (name, count) in &vertex.extents {
        let Some(other) = fragment.extents.get(name) else {
            continue;
        };
        if other == count {
            continue;
        }
        split.insert(
            (name.clone(), *count, *other),
            Split {
                pairs: BTreeSet::from([(0, 0)]),
                passes: BTreeSet::from(["Pass7@"]),
                vs_members: vertex.members.get(name).cloned().unwrap_or_default(),
                ps_members: fragment.members.get(name).cloned().unwrap_or_default(),
                vs_span: vertex.span(name),
                ps_span: fragment.span(name),
            },
        );
    }
    let mut described: BTreeMap<String, Described> = BTreeMap::new();
    for (name, one) in &vertex.members {
        let Some(other) = fragment.members.get(name) else {
            continue;
        };
        if one == other {
            continue;
        }
        described.insert(
            name.clone(),
            Described { vs: one.clone(), ps: other.clone(), pairs: 1 },
        );
    }
    report("stars", 1, 1, &split, &described);
}
