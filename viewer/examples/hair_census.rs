//! Every `hair.shpk` material in the install, by the clips its two G passes state and by how much of
//! its normal map's alpha reaches each of them.
//!
//! `hair_census [limit]`

use std::collections::BTreeMap;
use std::io::Read;

use ironworks::Ironworks;
use ironworks::file::File as _;
use ironworks::file::mtrl::Material;
use ironworks::file::tex;
use ironworks::sqpack::{Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const CHARA: u8 = 4;

const VERSION: u32 = 0x0103_0000;

const NORMAL: [u32; 2] = [0x0C5E_C1F1, 0xAAB4_D9E9];

const ALPHA_THRESHOLD: u32 = 0x29AC_0223;

/// What the semi-transparent G pass clips at, hardcoded in the shader.
const BLENDED: f32 = 16.0 / 255.0;

#[derive(Default)]
struct Tally {
    materials: usize,
    decoded: usize,
    /// Maps with nothing at all above the opaque clip, which draw nothing today.
    absent: usize,
    /// The share of texels in the band between the two clips, summed.
    band: f64,
    under_opaque: f64,
}

/// What the material is for, read off the map it binds: every hair normal map is `<part>_norm.tex`
/// or `<part>_n.tex`, and the part is what tells a scalp from a pair of eyebrows.
fn part(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem = stem
        .strip_suffix("_norm")
        .or_else(|| stem.strip_suffix("_n"))
        .unwrap_or(stem);
    stem.rsplit_once('_')
        .map_or_else(|| stem.to_owned(), |(_, tail)| format!("_{tail}"))
}

fn main() {
    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let limit: usize = std::env::args()
        .nth(1)
        .and_then(|held| held.parse().ok())
        .unwrap_or(usize::MAX);

    let entries = sqpack.entries().expect("the install's index");
    let mut groups: BTreeMap<String, Tally> = BTreeMap::new();
    let mut clips: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen = 0usize;

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
        seen += 1;
        let opaque = held
            .constants()
            .iter()
            .find(|constant| constant.id() == ALPHA_THRESHOLD)
            .and_then(|constant| held.constant_values(constant)?.first().copied())
            .unwrap_or(0.0);
        *clips.entry(format!("{opaque:.4}")).or_default() += 1;
        let path = held
            .samplers()
            .iter()
            .find(|sampler| NORMAL.contains(&sampler.id()))
            .and_then(|sampler| sampler.texture_index())
            .and_then(|index| held.textures().get(usize::from(index)))
            .map(|texture| texture.path().trim_start_matches("--").to_owned());
        let Some(path) = path else {
            continue;
        };
        let group = groups.entry(part(&path)).or_default();
        group.materials += 1;
        let Ok(texture) = ironworks.file::<tex::Texture>(&path) else {
            continue;
        };
        let Ok(image) = viewer::utils::tex_loader::decode_stack(&texture, 0, &path) else {
            continue;
        };
        let image = image.to_rgba8();
        let (mut opaque_out, mut blended_out, mut count) = (0u64, 0u64, 0u64);
        for pixel in image.pixels() {
            let alpha = f32::from(pixel.0[3]) / 255.0;
            opaque_out += u64::from(alpha >= opaque);
            blended_out += u64::from(alpha >= BLENDED);
            count += 1;
        }
        let count = count.max(1) as f64;
        group.decoded += 1;
        group.absent += usize::from(opaque_out == 0);
        group.band += (blended_out - opaque_out) as f64 / count;
        group.under_opaque += (count - opaque_out as f64) / count;
    }

    println!("{seen} hair.shpk materials");
    println!("stated opaque clips:");
    for (value, count) in &clips {
        println!("  {value} x{count}");
    }
    println!(
        "{:<8} {:>7} {:>8} {:>8} {:>10} {:>10}",
        "part", "mtrl", "decoded", "blank", "mean band", "mean under"
    );
    for (name, tally) in &groups {
        let decoded = tally.decoded.max(1) as f64;
        println!(
            "{:<8} {:>7} {:>8} {:>8} {:>9.2}% {:>9.2}%",
            name,
            tally.materials,
            tally.decoded,
            tally.absent,
            100.0 * tally.band / decoded,
            100.0 * tally.under_opaque / decoded,
        );
    }
}
