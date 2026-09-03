//! Walks a zone's grass the way the scene does, to check against real files that a count slot
//! resolves to the model its zone names and that the placements line up behind it.

use ironworks::{
    Ironworks,
    file::{ggd, gzd},
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const ZONE: &str = "bg/ffxiv/sea_s1/fld/s1f2/grass";

fn main() {
    let sqpack = std::env::args().nth(1).unwrap_or_else(|| SQPACK.to_owned());
    let zone = std::env::args().nth(2).unwrap_or_else(|| ZONE.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));

    let file: gzd::GrassZone = ironworks
        .file(&format!("{zone}/grass_zone_data.gzd"))
        .expect("the zone holds no grass file");
    let models = file.model_paths();
    println!(
        "{} models, {} slots",
        models.len(),
        file.model_slot_capacity()
    );
    for (at, name) in models.iter().enumerate() {
        println!("  slot {:>2}  {name}", at + ggd::Chunk::AUTO_LAYERS);
    }
    for (at, name) in file.color_map().iter().enumerate() {
        println!("  auto {at}  color map {name:?}");
    }

    let mut read = 0;
    let mut placed = 0;
    let mut auto = 0;
    let mut missing = Vec::new();
    for detail in [gzd::Detail::High, gzd::Detail::Medium, gzd::Detail::Low] {
        let grids = file.grids(detail);
        println!("\n{:?}: {} grids", detail, grids.len());
        for grid in grids {
            let Ok(held) = ironworks.file::<ggd::GrassGrid>(&format!("{zone}/{}", grid.file()))
            else {
                missing.push(grid.file());
                continue;
            };
            read += 1;
            let mut here = 0;
            let origin = held.world_origin();
            for chunk in held.chunks() {
                let mut at = 0;
                for (slot, count) in chunk.counts().iter().enumerate() {
                    let count = usize::from(*count);
                    match slot.checked_sub(ggd::Chunk::AUTO_LAYERS) {
                        Some(slot) if count > 0 => {
                            assert!(models.get(slot).is_some(), "slot {slot} names no model");
                            placed += count;
                            here += count;
                        }
                        Some(_) => {}
                        None => auto += count,
                    }
                    at += count;
                }
                // The one thing a wrong cursor breaks: the counts have to spend the chunk exactly.
                assert_eq!(
                    at,
                    chunk.placements().len(),
                    "counts do not sum to placements"
                );
            }
            if here > 0 {
                println!(
                    "   {here:>4} at {:9.1} {:7.1} {:9.1}   {}",
                    origin[0],
                    origin[1],
                    origin[2],
                    grid.file()
                );
            }
        }
    }
    println!(
        "\n{read} grids read, {} missing\n{placed} placements resolve to a model, {auto} are auto layers",
        missing.len()
    );
}
