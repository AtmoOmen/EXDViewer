//! Scratch tool: an avfx model's own vertex bounds, to see what local space its geometry sits in.
//!
//! `avfx_model_bounds <path.avfx>`

use ironworks::{
    Ironworks,
    file::File,
    file::avfx::Avfx,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let sqpack = std::env::var("SQPACK").unwrap_or_else(|_| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    let path = std::env::args().nth(1).expect("path");
    let bytes: Vec<u8> = ironworks.file(&path).expect("read");
    let avfx = Avfx::read(std::io::Cursor::new(bytes)).expect("parse");

    for (i, model) in avfx.models().iter().enumerate() {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for vertex in model.vertices() {
            let position = vertex.position();
            for lane in 0..3 {
                min[lane] = min[lane].min(position[lane]);
                max[lane] = max[lane].max(position[lane]);
            }
        }
        println!(
            "model[{i}]: {} vertices, {} triangles, min={min:?} max={max:?}",
            model.vertices().len(),
            model.triangles().len(),
        );
        if i == 0 || i == 1 || i == 7 {
            for vertex in model.vertices().iter().take(4) {
                println!("  {:?} normal={:?} uv={:?}", vertex.position(), vertex.normal(), vertex.uv());
            }
        }
    }
}
