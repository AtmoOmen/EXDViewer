//! What a texture holds, band by band down its height, so a gradient can be read as numbers.
//!
//! `tex_probe <path.tex> [bands]`

use ironworks::file::tex;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a texture path");
    let bands: u32 = args.next().and_then(|held| held.parse().ok()).unwrap_or(8);

    let texture: tex::Texture = ironworks.file(&path).expect("texture");
    let image = viewer::utils::tex_loader::decode_stack(&texture, 0, &path)
        .expect("decode")
        .to_rgba8();
    let (wide, tall) = (image.width(), image.height());
    println!("{path}  {wide} x {tall}");
    for band in 0..bands {
        let from = tall * band / bands;
        let to = (tall * (band + 1) / bands).max(from + 1);
        let mut sum = [0u64; 4];
        let mut count = 0u64;
        for y in from..to.min(tall) {
            for x in 0..wide {
                let held = image.get_pixel(x, y).0;
                for (at, channel) in held.iter().enumerate() {
                    sum[at] += u64::from(*channel);
                }
                count += 1;
            }
        }
        let mean = sum.map(|held| (held / count.max(1)) as u8);
        println!(
            "  rows {from:>5}..{to:<5} mean rgba {:>3} {:>3} {:>3} {:>3}",
            mean[0], mean[1], mean[2], mean[3]
        );
    }
}
