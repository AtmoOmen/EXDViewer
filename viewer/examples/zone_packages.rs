//! Which shader packages a zone's materials name, and how many materials name each.
//!
//! `zone_packages bg/ffxiv/sea_s1/fld/s1f2`

use std::collections::BTreeMap;

use ironworks::file::mtrl::Material;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).expect("the path list");
    for zone in std::env::args().skip(1) {
        let mut tally: BTreeMap<String, usize> = BTreeMap::new();
        let mut read = 0;
        for path in list.lines() {
            if !path.starts_with(&zone) || !path.ends_with(".mtrl") {
                continue;
            }
            let Ok(material) = ironworks.file::<Material>(path) else {
                continue;
            };
            read += 1;
            *tally.entry(material.shader().to_owned()).or_default() += 1;
        }
        println!("== {zone}: {read} materials");
        for (name, count) in &tally {
            println!("   {count:>4}  {name}");
        }
    }
}
