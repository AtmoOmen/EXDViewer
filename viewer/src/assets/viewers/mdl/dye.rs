//! Replacing a color table's own values with a stain's, the way a worn piece's dye table names.
//!
//! `chara/base_material/stainingtemplate.stm` holds the legacy, two-scalar templates a legacy color
//! table's dye rows point at; `stainingtemplate_gud.stm` holds the modern, nine-scalar ones an
//! extended table's dye rows point at. Both are read once and kept together, since a dye row's
//! template id alone does not say which file it lives in.

use std::io::Cursor;
use std::sync::Arc;

use anyhow::Result;
use half::f16;
use ironworks::file::{File, mtrl, stm};

use super::Table;
use crate::backend::Backend;

const REGULAR: &str = "chara/base_material/stainingtemplate.stm";
const GOOD: &str = "chara/base_material/stainingtemplate_gud.stm";

/// Both staining template files the game ships.
pub struct Templates {
    regular: stm::StainingTemplates,
    good: stm::StainingTemplates,
}

impl Templates {
    pub async fn read(backend: &Backend) -> Result<Self> {
        let files = backend.files();
        let regular = files.read(REGULAR).await?;
        let good = files.read(GOOD).await?;
        Ok(Self {
            regular: stm::StainingTemplates::read(Cursor::new(regular))?,
            good: stm::StainingTemplates::read(Cursor::new(good))?,
        })
    }

    /// A stain's values for the template a dye row names, trying both files since a row's id alone
    /// does not say which it came from.
    fn pack(&self, template: u16, stain: u8) -> Option<stm::DyePack> {
        let template = u32::from(template);
        self.regular
            .template(template)
            .or_else(|| self.good.template(template))?
            .dye(stain)
    }
}

/// Where diffuse, specular and emissive sit in the extended row layout every shader addresses.
/// Unchanged by the legacy crossover: only halves 3 and 7 swap meaning between the layouts.
const COLORS: [(mtrl::DyeField, fn(&stm::DyePack) -> [f32; 3], [usize; 3]); 3] = [
    (mtrl::DyeField::Diffuse, |pack| pack.diffuse, [0, 1, 2]),
    (mtrl::DyeField::Specular, |pack| pack.specular, [4, 5, 6]),
    (mtrl::DyeField::Emissive, |pack| pack.emissive, [8, 9, 10]),
];

/// The scalars a modern, nine-scalar template carries, which only an extended table's dye row can
/// name: a legacy row's five bits reach no further than [`mtrl::DyeField::Metalness`].
const SCALARS: [(mtrl::DyeField, fn(&stm::DyePack) -> f32, usize); 7] = [
    (mtrl::DyeField::Roughness, |pack| pack.roughness, 16),
    (mtrl::DyeField::SheenRate, |pack| pack.sheen_rate, 12),
    (mtrl::DyeField::SheenTint, |pack| pack.sheen_tint, 13),
    (mtrl::DyeField::SheenAperture, |pack| pack.sheen_aperture, 14),
    (mtrl::DyeField::Anisotropy, |pack| pack.anisotropy, 19),
    (
        mtrl::DyeField::SphereIndex,
        |pack| f32::from(pack.sphere_index),
        27,
    ),
    (mtrl::DyeField::SphereMask, |pack| pack.sphere_mask, 21),
];

/// Where [`mtrl::DyeField::Scalar3`] and [`mtrl::DyeField::Metalness`] land, which is the one place
/// legacy and extended disagree: `program::table` widens a legacy row's shininess (half 7) onto
/// extended half 3, and its specular mask (half 3) onto extended half 7, so a legacy dye row's
/// second scalar has to follow the specular mask there rather than to the modern metalness slot.
fn scalar3_and_metalness(kind: mtrl::ColorTableKind) -> [(mtrl::DyeField, usize); 2] {
    match kind {
        mtrl::ColorTableKind::Legacy => {
            [(mtrl::DyeField::Scalar3, 3), (mtrl::DyeField::Metalness, 7)]
        }
        _ => [
            (mtrl::DyeField::Scalar3, 3),
            (mtrl::DyeField::Metalness, 18),
        ],
    }
}

/// Replaces the fields a dye row names in one row of a color table already widened to the extended
/// layout, which is a no-op for a field the row does not dye.
fn apply(row: &mut [u16], kind: mtrl::ColorTableKind, dye: mtrl::DyeRow, pack: &stm::DyePack) {
    let half = |v: f32| f16::from_f32(v).to_bits();
    for (field, read, offsets) in COLORS {
        if !dye.dyes(field) {
            continue;
        }
        for (offset, v) in offsets.into_iter().zip(read(pack)) {
            if let Some(slot) = row.get_mut(offset) {
                *slot = half(v);
            }
        }
    }
    for (field, offset) in scalar3_and_metalness(kind) {
        if dye.dyes(field)
            && let Some(slot) = row.get_mut(offset)
        {
            *slot = half(match field {
                mtrl::DyeField::Scalar3 => pack.scalar3,
                _ => pack.metalness,
            });
        }
    }
    for (field, read, offset) in SCALARS {
        if dye.dyes(field)
            && let Some(slot) = row.get_mut(offset)
        {
            *slot = half(read(pack));
        }
    }
}

/// Where a shader addresses every row of the layout [`super::program::table`] widens a legacy table
/// into, halves per row.
const ROW: usize = 32;

/// The color table the game's own shaders read, with the wearer's stains replacing whatever fields
/// the material's own dye table names. `None` where nothing was picked or nothing in the table is
/// dyed, so the caller keeps drawing the table it already has.
pub fn table(
    base: &Table,
    colors: &mtrl::ColorTable,
    templates: &Templates,
    stains: [Option<u8>; 2],
) -> Option<Table> {
    if stains == [None, None] {
        return None;
    }
    let (raw, columns, rows) = &**base;
    let mut values = raw.clone();
    let mut dyed = false;
    for index in 0..colors.rows() {
        let Some(row) = colors.dye_row(index) else {
            continue;
        };
        if row.template() == 0 {
            continue;
        }
        let Some(Some(stain)) = stains.get(usize::from(row.channel())) else {
            continue;
        };
        let Some(pack) = templates.pack(row.template(), *stain) else {
            continue;
        };
        let targets: &[usize] = match colors.kind() {
            mtrl::ColorTableKind::Legacy => &[index * 2, index * 2 + 1],
            _ => &[index],
        };
        for &target in targets {
            let Some(slice) = values.get_mut(target * ROW..(target + 1) * ROW) else {
                continue;
            };
            apply(slice, colors.kind(), row, &pack);
        }
        dyed = true;
    }
    dyed.then(|| Arc::new((values, *columns, *rows)))
}
