//! Where a placed model's surface material comes from.
//!
//! `mdl_extra lgb <path.lgb> [substring]`   bg parts, their collision mode and material words
//! `mdl_extra sound <path.lgb>`             sound emitters and the files they name
//! `mdl_extra mdl <path.mdl>`               every declared span of a model, against its length
//! `mdl_extra pcb <path.pcb>`               the material word on every collision triangle

use std::collections::BTreeMap;

use ironworks::file::layer::{Instance, InstanceData};
use ironworks::file::pcb::{Collision, Node};
use ironworks::file::{lgb::LayerGroupFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const SURFACES: [&str; 16] = [
    "none", "dart", "grass", "sand", "stone", "wood", "metal", "gravel", "leaf", "powder",
    "carpet", "snow", "water1", "water2", "mesh", "sticky",
];

fn surface(word: u64) -> String {
    let low = (word & 0xff) as usize;
    match SURFACES.get(low) {
        Some(name) => format!("{low:#04x} {name}"),
        None => format!("{low:#04x} ?"),
    }
}

fn stem(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

fn bgparts(
    ironworks: &Ironworks<SqPack<Install>>,
    instances: &[Instance],
    under: &str,
    filter: &str,
    depth: usize,
    tally: &mut BTreeMap<(String, u64, u64, i32), usize>,
) {
    for instance in instances {
        match instance.data() {
            InstanceData::BgPart(held) if !held.asset_path().is_empty() => {
                let name = stem(held.asset_path());
                let kind: &str = &format!("{:?}", held.collision());
                if name.contains(filter) {
                    let at = instance.transform().translation();
                    println!(
                        "  {name:24} at {:8.1} {:7.1} {:8.1}  collision {kind:8} id {:#018x} = {}  mask {:#018x}  pcb {:?}  under {under}",
                        at[0], at[1], at[2],
                        held.collision_material_id(),
                        surface(held.collision_material_id()),
                        held.collision_material_mask(),
                        held.collision_asset_path(),
                    );
                }
                let key = name
                    .split('_')
                    .nth(2)
                    .unwrap_or(&name)
                    .trim_end_matches(|c: char| c.is_ascii_digit() || c.is_ascii_alphabetic())
                    .to_owned();
                let key = if key.is_empty() {
                    name.split('_').nth(2).unwrap_or(&name)[..3.min(name.len())].to_owned()
                } else {
                    key
                };
                *tally
                    .entry((
                        key,
                        held.collision_material_id() & 0xff,
                        held.collision_material_mask() & 0xffff,
                        held.collision() as i32,
                    ))
                    .or_default() += 1;
            }
            InstanceData::SharedGroup(held) if depth < 6 && !held.asset_path().is_empty() => {
                let Ok(file) = ironworks.file::<SharedGroupFile>(held.asset_path()) else {
                    continue;
                };
                for group in file.scene().layer_groups() {
                    for layer in group.layers() {
                        bgparts(
                            ironworks,
                            layer.instances(),
                            held.asset_path(),
                            filter,
                            depth + 1,
                            tally,
                        );
                    }
                }
            }
            _ => (),
        }
    }
}

fn triangles(node: &Node, tally: &mut BTreeMap<u64, usize>) {
    for primitive in node.primitives() {
        *tally.entry(primitive.material()).or_default() += 1;
    }
    for child in node.children() {
        triangles(child, tally);
    }
}

/// Every span the model header declares, in the order the file writes them.
fn spans(bytes: &[u8]) -> Vec<(usize, usize, String)> {
    let u8_at = |at: usize| bytes[at] as usize;
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let u32_at = |at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };

    let mut out = Vec::new();
    let mut at = 0;
    let mut push = |at: &mut usize, size: usize, what: &str| {
        out.push((*at, size, what.to_owned()));
        *at += size;
    };

    let declarations = u16_at(0x0c);
    push(&mut at, 0x44, "file header");
    push(&mut at, declarations * 17 * 8, "vertex declarations");

    let string_count = u16_at(at);
    let string_size = u32_at(at + 4);
    push(&mut at, 8, "string table header");
    push(&mut at, string_size, &format!("{string_count} strings"));

    let model = at;
    let mesh_count = u16_at(model + 4);
    let attribute_count = u16_at(model + 6);
    let submesh_count = u16_at(model + 8);
    let material_count = u16_at(model + 10);
    let bone_count = u16_at(model + 12);
    let bone_table_count = u16_at(model + 14);
    let shape_count = u16_at(model + 16);
    let shape_mesh_count = u16_at(model + 18);
    let shape_value_count = u16_at(model + 20);
    let flags2 = bytes[model + 23];
    let element_id_count = u16_at(model + 24);
    let terrain_shadow_mesh_count = u8_at(model + 26);
    let culling_grid_count = u16_at(model + 36);
    let terrain_shadow_submesh_count = u16_at(model + 38);
    let neck_morph_count = u8_at(model + 43);
    let bone_table_array = u16_at(model + 44);
    let face_data_count = u16_at(model + 48);
    push(&mut at, 0x38, "model header");

    push(&mut at, element_id_count * 32, "element ids");
    push(&mut at, 3 * 60, "lods");
    if flags2 & 0x10 != 0 {
        push(&mut at, 3 * 40, "extra lods");
    }
    push(&mut at, mesh_count * 36, "meshes");
    push(&mut at, attribute_count * 4, "attribute name offsets");
    push(
        &mut at,
        terrain_shadow_mesh_count * 20,
        "terrain shadow meshes",
    );
    push(&mut at, submesh_count * 16, "submeshes");
    push(
        &mut at,
        terrain_shadow_submesh_count * 10,
        "terrain shadow submeshes",
    );
    push(&mut at, material_count * 4, "material name offsets");
    push(&mut at, bone_count * 4, "bone name offsets");
    push(&mut at, bone_table_count * 4, "bone tables");
    push(&mut at, bone_table_array * 2, "bone table indices");
    push(&mut at, shape_count * 16, "shapes");
    push(&mut at, shape_mesh_count * 12, "shape meshes");
    push(&mut at, shape_value_count * 4, "shape values");

    let bone_map = u32_at(at);
    push(&mut at, 4, "submesh bone map size");
    push(&mut at, bone_map, "submesh bone map");
    push(&mut at, neck_morph_count * 40, "neck morphs");
    push(&mut at, face_data_count * 16, "face data");

    let padding = u8_at(at);
    push(&mut at, 1 + padding, "padding run");
    push(&mut at, 4 * 32, "four bounding boxes");
    push(&mut at, bone_count * 32, "bone bounding boxes");
    push(&mut at, culling_grid_count * 32, "culling grid");

    for lod in 0..3 {
        let vertex = u32_at(0x10 + lod * 4);
        let index = u32_at(0x1c + lod * 4);
        let vertex_size = u32_at(0x28 + lod * 4);
        let index_size = u32_at(0x34 + lod * 4);
        if vertex_size > 0 {
            out.push((vertex, vertex_size, format!("lod {lod} vertices")));
        }
        if index_size > 0 {
            out.push((index, index_size, format!("lod {lod} indices")));
        }
    }
    out.sort_by_key(|held| held.0);
    out
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("a mode");
    let path = args.next().expect("a path");

    match mode.as_str() {
        "lgb" => {
            let filter = args.next().unwrap_or_default();
            let file: LayerGroupFile = ironworks.file(&path).unwrap();
            let mut tally = BTreeMap::new();
            for layer in file.group().layers() {
                bgparts(
                    &ironworks,
                    layer.instances(),
                    layer.name(),
                    &filter,
                    0,
                    &mut tally,
                );
            }
            println!("== stem, material id, mask, collision mode -> instances");
            for ((key, id, mask, kind), count) in &tally {
                println!(
                    "  {key:8} {:>14}  mask {mask:#06x}  mode {kind}  x{count}",
                    surface(*id)
                );
            }
        }
        "sound" => {
            let file: LayerGroupFile = ironworks.file(&path).unwrap();
            for layer in file.group().layers() {
                for instance in layer.instances() {
                    if let InstanceData::Sound(held) = instance.data() {
                        println!(
                            "  {:?}  auto {}  {}  ({} bytes of shape)",
                            held.kind(),
                            held.auto_play(),
                            held.asset_path(),
                            held.binary().len()
                        );
                    }
                }
            }
        }
        "mdl" => {
            let bytes: Vec<u8> = ironworks.file(&path).unwrap();
            println!("== {path}: {} bytes", bytes.len());
            let mut covered = 0;
            let mut end = 0;
            for (at, size, what) in spans(&bytes) {
                if at > end {
                    println!("  {end:#010x}  {:>8}  UNCLAIMED", at - end);
                }
                println!("  {at:#010x}  {size:>8}  {what}");
                covered += size;
                end = end.max(at + size);
            }
            if end < bytes.len() {
                println!("  {end:#010x}  {:>8}  UNCLAIMED (tail)", bytes.len() - end);
            }
            println!("  claimed {covered} of {}", bytes.len());
        }
        "pcb" => {
            let file: Collision = ironworks.file(&path).unwrap();
            let Collision::Mesh(mesh) = file else {
                let Collision::List(list) = file else { return };
                for entry in list.entries() {
                    println!(
                        "  tr{:04}  x {:8.1}..{:8.1}  y {:7.1}..{:7.1}  z {:8.1}..{:8.1}",
                        entry.id(),
                        entry.bounds().min()[0],
                        entry.bounds().max()[0],
                        entry.bounds().min()[1],
                        entry.bounds().max()[1],
                        entry.bounds().min()[2],
                        entry.bounds().max()[2],
                    );
                }
                return;
            };
            let mut tally = BTreeMap::new();
            triangles(mesh.root(), &mut tally);
            for (word, count) in tally {
                println!("  {word:#018x} = {:>14}  x{count}", surface(word));
            }
        }
        "sweep" => {
            let mut tally = BTreeMap::new();
            let mut read = 0;
            for one in std::fs::read_to_string(&path).unwrap().lines() {
                let Ok(file) = ironworks.file::<Collision>(one) else {
                    continue;
                };
                read += 1;
                if let Collision::Mesh(mesh) = file {
                    triangles(mesh.root(), &mut tally);
                }
            }
            println!("== {read} meshes");
            for (word, count) in tally {
                println!("  {word:#018x} = {:>14}  x{count}", surface(word));
            }
        }
        other => println!("unknown mode {other}"),
    }
}
