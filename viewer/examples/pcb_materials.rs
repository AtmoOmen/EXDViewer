//! The material words the shipped collision meshes state, read the way the crate reads them and
//! the way it read them before the width was measured off the node extents. The meshes a `list.pcb`
//! names are counted apart, since those are the ones the game still reaches.
//!
//! `pcb_materials <paths file> [limit]`

use std::collections::BTreeMap;
use std::io::Cursor;

use ironworks::file::{File, pcb};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

type Words = BTreeMap<u64, usize>;

fn collect(node: &pcb::Node, into: &mut Words) {
    for primitive in node.primitives() {
        *into.entry(primitive.material()).or_default() += 1;
    }
    for child in node.children() {
        collect(child, into);
    }
}

/// Every material word a mesh states, and the words a wide reading of it would have stated.
fn words(ironworks: &Ironworks<SqPack<Install>>, path: &str) -> Option<(Words, Words)> {
    let bytes = ironworks.file::<Vec<u8>>(path).ok()?;
    let pcb::Collision::Mesh(mesh) = pcb::Collision::read(Cursor::new(bytes.clone())).ok()? else {
        return None;
    };
    let mut measured = Words::new();
    collect(mesh.root(), &mut measured);

    let mut forced = Words::new();
    if let Ok(wide) = pcb::Mesh::read_with(Cursor::new(bytes), pcb::MaterialWidth::Wide) {
        collect(wide.root(), &mut forced);
    }
    Some((measured, forced))
}

fn report(name: &str, held: &Words) {
    let total: usize = held.values().sum();
    let unnamed: usize = held
        .iter()
        .filter(|(word, _)| **word & 0xff != 0 && pcb::surface(**word).is_none())
        .map(|(_, count)| count)
        .sum();
    println!(
        "{name}: {} words over {total} triangles, widest {:#x}, {unnamed} on a surface nothing names",
        held.len(),
        held.keys().next_back().unwrap_or(&0),
    );
    let mut rows: Vec<_> = held.iter().collect();
    rows.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (word, count) in rows.iter().take(8) {
        println!("   {word:#018x}  {count}");
    }
}

/// The meshes a list names, which live beside it.
fn named(ironworks: &Ironworks<SqPack<Install>>, path: &str) -> Vec<String> {
    let Ok(pcb::Collision::List(list)) = ironworks
        .file::<Vec<u8>>(path)
        .and_then(|bytes| pcb::Collision::read(Cursor::new(bytes)))
    else {
        return Vec::new();
    };
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    list.entries()
        .iter()
        .map(|entry| format!("{dir}/{}", pcb::MeshList::mesh_file(entry.id())))
        .collect()
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a paths file");
    let limit = std::env::args()
        .nth(2)
        .and_then(|held| held.parse().ok())
        .unwrap_or(usize::MAX);
    let paths = std::fs::read_to_string(list).expect("the paths file");

    let mut measured = Words::new();
    let mut forced = Words::new();
    let mut meshes = 0usize;
    let mut apart = 0usize;
    for path in paths.lines().take(limit) {
        let Some((here, there)) = words(&ironworks, path) else {
            continue;
        };
        meshes += 1;
        apart += usize::from(here != there);
        for (word, count) in here {
            *measured.entry(word).or_default() += count;
        }
        for (word, count) in there {
            *forced.entry(word).or_default() += count;
        }
    }
    println!("{meshes} meshes, {apart} the wide reading disagrees with");
    report("measured", &measured);
    report("forced wide", &forced);

    let mut reached = 0usize;
    let mut narrow = Vec::new();
    for path in paths.lines().filter(|path| path.ends_with("/list.pcb")) {
        for mesh in named(&ironworks, path) {
            let Some((here, there)) = words(&ironworks, &mesh) else {
                continue;
            };
            reached += 1;
            if here != there {
                narrow.push(mesh);
            }
        }
    }
    println!(
        "{reached} meshes a list names, {} of them not written wide: {}",
        narrow.len(),
        narrow.join(" ")
    );
}
