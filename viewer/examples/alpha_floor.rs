//! What every material states as its own alpha threshold, so the floor the viewer puts under one
//! can be told from what the files say.
//!
//! `alpha_floor`

use std::collections::BTreeMap;
use std::io::Read;

use ironworks::file::File as _;
use ironworks::file::mtrl::Material;
use ironworks::sqpack::{Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// Where character files sit.
const CHARA: u8 = 4;

const VERSION: u32 = 0x0103_0000;

const ALPHA_THRESHOLD: u32 = 0x29AC_0223;

const NORMAL: [u32; 2] = [0x0C5E_C1F1, 0xAAB4_D9E9];

fn main() {
    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    let entries = sqpack.entries().expect("the install's index");
    let chara: Vec<_> = entries
        .iter()
        .filter(|entry| entry.category == CHARA)
        .collect();
    println!("{} character entries of {}", chara.len(), entries.len());

    let (mut materials, mut stated) = (0usize, 0usize);
    let mut by_value: BTreeMap<String, usize> = BTreeMap::new();
    let mut floored: BTreeMap<String, usize> = BTreeMap::new();
    for entry in chara {
        let Ok(mut file) = sqpack.file_by_hash(entry.repository, entry.category, entry.hash) else {
            continue;
        };
        let mut head = [0u8; 4];
        if file.read_exact(&mut head).is_err() || u32::from_le_bytes(head) != VERSION {
            continue;
        }
        let mut bytes = head.to_vec();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let Ok(held) = Material::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        materials += 1;
        let normal = held
            .samplers()
            .iter()
            .any(|sampler| NORMAL.contains(&sampler.id()) && sampler.texture_index().is_some());
        let declared = held
            .constants()
            .iter()
            .find(|constant| constant.id() == ALPHA_THRESHOLD)
            .and_then(|constant| held.constant_values(constant)?.first().copied());
        let Some(declared) = declared else {
            continue;
        };
        stated += 1;
        let shader = held.shader().to_owned();
        *by_value.entry(format!("{declared:.4}")).or_default() += 1;
        // The viewer's own test, verbatim: only a character family hides a cutout in the normal
        // map, and only those materials have their threshold floored.
        let character =
            shader.starts_with("character") || matches!(shader.as_str(), "skin.shpk" | "iris.shpk");
        if character && normal && declared > 0.0 && declared < 0.5 {
            *floored.entry(format!("{shader} {declared:.4}")).or_default() += 1;
        }
    }
    println!("{materials} materials, {stated} stating a threshold");
    for (value, count) in &by_value {
        println!("  {value} x{count}");
    }
    println!("floored by the viewer's 0.5:");
    for (what, count) in &floored {
        println!("  {what} x{count}");
    }
}
