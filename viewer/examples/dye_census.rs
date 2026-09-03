//! Corpus sweep over a sample of real materials: how many dye channels equipment actually uses,
//! and whether any legacy dye row sets a field past the two a legacy template carries.
//!
//! `dye_census <path list file>`

use ironworks::file::{mtrl, File};
use ironworks::{Ironworks, sqpack::{Install, SqPack}};

const SQPACK: &str = "/tmp/xiv/global/latest/game/sqpack";

fn main() {
    let list = std::env::args().nth(1).expect("a path list file");
    let paths = std::fs::read_to_string(&list).expect("path list");
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));

    let mut materials = 0u32;
    let mut with_table = 0u32;
    let mut with_dye = 0u32;
    let mut channels = [0u32; 4];
    let mut legacy_rows = 0u32;
    let mut legacy_bit4 = 0u32;
    let mut legacy_bits_past4 = 0u32;
    let mut extended_rows = 0u32;
    let mut extended_by_field: [u32; 12] = [0; 12];
    let fields = [
        mtrl::DyeField::Diffuse,
        mtrl::DyeField::Specular,
        mtrl::DyeField::Emissive,
        mtrl::DyeField::Scalar3,
        mtrl::DyeField::Metalness,
        mtrl::DyeField::Roughness,
        mtrl::DyeField::SheenRate,
        mtrl::DyeField::SheenTint,
        mtrl::DyeField::SheenAperture,
        mtrl::DyeField::Anisotropy,
        mtrl::DyeField::SphereIndex,
        mtrl::DyeField::SphereMask,
    ];

    for path in paths.lines() {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        let Ok(material) = mtrl::Material::read(std::io::Cursor::new(bytes)) else {
            continue;
        };
        materials += 1;
        let Some(table) = material.color_table() else {
            continue;
        };
        with_table += 1;
        let mut any_dye = false;
        for index in 0..table.rows() {
            let Some(row) = table.dye_row(index) else {
                continue;
            };
            if row.template() == 0 {
                continue;
            }
            any_dye = true;
            channels[usize::from(row.channel().min(3))] += 1;
            match table.kind() {
                mtrl::ColorTableKind::Legacy => {
                    legacy_rows += 1;
                    if row.dyes(mtrl::DyeField::Metalness) {
                        legacy_bit4 += 1;
                    }
                    for field in &fields[5..] {
                        if row.dyes(*field) {
                            legacy_bits_past4 += 1;
                        }
                    }
                }
                mtrl::ColorTableKind::Extended => {
                    extended_rows += 1;
                    for (i, field) in fields.iter().enumerate() {
                        if row.dyes(*field) {
                            extended_by_field[i] += 1;
                        }
                    }
                }
                mtrl::ColorTableKind::Unknown => {}
            }
        }
        if any_dye {
            with_dye += 1;
        }
    }

    println!("materials read: {materials}, with a color table: {with_table}, with any dye row: {with_dye}");
    println!("channel tally (index = DyeRow::channel()): {channels:?}");
    println!("legacy dye rows: {legacy_rows}, setting Metalness bit: {legacy_bit4}, setting any bit past Metalness: {legacy_bits_past4}");
    println!("extended dye rows: {extended_rows}");
    for (field, count) in fields.iter().zip(extended_by_field) {
        println!("  {field:?}: {count}");
    }
}
