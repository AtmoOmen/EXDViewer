//! What a hair normal map's alpha does down its mip chain, so the clip a distant strand is tested
//! against can be told from the one at the surface.
//!
//! `hair_mips [limit]`

use std::io::Read;

use ironworks::file::File as _;
use ironworks::file::mtrl::Material;
use ironworks::file::tex;
use ironworks::sqpack::{Install, SqPack};
use ironworks::Ironworks;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const CHARA: u8 = 4;

const VERSION: u32 = 0x0103_0000;

const NORMAL: [u32; 2] = [0x0C5E_C1F1, 0xAAB4_D9E9];

/// The two clips `hair.shpk` states: its opaque pass and its semi-transparent one.
const OPAQUE: f32 = 0.75;
const BLENDED: f32 = 16.0 / 255.0;

fn main() {
    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let limit: usize = std::env::args()
        .nth(1)
        .and_then(|held| held.parse().ok())
        .unwrap_or(12);

    let entries = sqpack.entries().expect("the install's index");
    let mut seen = 0usize;
    // Summed over every map so the chain's trend is read off a corpus, not off one texture.
    let mut totals: Vec<(u64, u64, u64)> = Vec::new();

    for entry in entries.iter().filter(|entry| entry.category == CHARA) {
        if seen >= limit {
            break;
        }
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
        if held.shader() != "hair.shpk" {
            continue;
        }
        let Some(index) = held
            .samplers()
            .iter()
            .find(|sampler| NORMAL.contains(&sampler.id()))
            .and_then(|sampler| sampler.texture_index())
        else {
            continue;
        };
        let Some(path) = held.textures().get(usize::from(index)).map(|held| held.path()) else {
            continue;
        };
        let path = path.trim_start_matches("--").to_owned();
        let Ok(texture) = ironworks.file::<tex::Texture>(&path) else {
            continue;
        };
        seen += 1;
        println!("{path}");
        for level in 0u8..16 {
            if texture.mip_offset(level).is_none() {
                break;
            }
            let Ok(image) = viewer::utils::tex_loader::decode_stack(&texture, level, &path) else {
                break;
            };
            let image = image.to_rgba8();
            let (mut under_opaque, mut under_blended, mut count) = (0u64, 0u64, 0u64);
            for pixel in image.pixels() {
                let alpha = f32::from(pixel.0[3]) / 255.0;
                under_opaque += u64::from(alpha < OPAQUE);
                under_blended += u64::from(alpha < BLENDED);
                count += 1;
            }
            if totals.len() <= usize::from(level) {
                totals.push((0, 0, 0));
            }
            let slot = &mut totals[usize::from(level)];
            slot.0 += under_opaque;
            slot.1 += under_blended;
            slot.2 += count;
            let (wide, tall) = texture.mip_size(level);
            println!(
                "  mip {level:>2}  {wide:>5}x{tall:<5}  under 0.75 {:>6.2}%   under 16/255 {:>6.2}%",
                100.0 * under_opaque as f64 / count.max(1) as f64,
                100.0 * under_blended as f64 / count.max(1) as f64,
            );
        }
    }
    println!("\n{seen} hair normal maps, summed by level:");
    for (level, (opaque, blended, count)) in totals.iter().enumerate() {
        println!(
            "  mip {level:>2}  under 0.75 {:>6.2}%   under 16/255 {:>6.2}%   ({count} texels)",
            100.0 * *opaque as f64 / (*count).max(1) as f64,
            100.0 * *blended as f64 / (*count).max(1) as f64,
        );
    }
}
