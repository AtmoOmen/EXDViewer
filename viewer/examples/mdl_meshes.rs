//! Every mesh and submesh a model declares, at each detail level: how many indices it draws and
//! which material shades it.
//!
//! `mdl_meshes <path.mdl> ...`

use ironworks::file::mdl::{Lod, ModelContainer};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let Ok(container) = ironworks.file::<ModelContainer>(&path) else {
            println!("MISS {path}");
            continue;
        };
        println!("== {path} shadowing {}", container.model(Lod::High).shadowing());
        for lod in [Lod::High, Lod::Medium, Lod::Low] {
            for (index, mesh) in container.model(lod).meshes().into_iter().enumerate() {
                let material = mesh.material().unwrap_or_default();
                let count = mesh.indices().map(|held| held.len()).unwrap_or(0);
                let parts: Vec<usize> = mesh.submeshes().iter().map(|part| part.count).collect();
                println!("  lod {lod:?} mesh {index:3} {count:8} indices  {parts:?}  {material}");
            }
        }
    }
}
