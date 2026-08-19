//! What a material states: its package, its keys, the constants it overrides and the textures it
//! binds to each sampler.
//!
//! `mtrl_dump <path.mtrl>`

use ironworks::file::mtrl::Material;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn named(id: u32) -> String {
    shaders::names::resolve(id).map_or_else(|| format!("{id:08x}"), ToOwned::to_owned)
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let held: Material = ironworks.file(&path).expect("material");
        println!("== {path}\n{}", held.shader());
        for key in held.shader_keys() {
            println!("  key {} = {}", named(key.category()), named(key.value()));
        }
        for constant in held.constants() {
            println!(
                "  constant {} at +{} = {:?}",
                named(constant.id()),
                constant.value_offset(),
                held.constant_values(constant),
            );
        }
        if let Some(table) = held.color_table() {
            println!("  color table {:?} {} rows", table.kind(), table.rows());
            for at in 0..table.rows() {
                if let Some(row) = table.row_values(at) {
                    println!(
                        "    row {at:>2} diffuse {:?} emissive {:?} specular {:?}",
                        row.diffuse.map(|held| (held * 100.0).round() / 100.0),
                        row.emissive.map(|held| (held * 100.0).round() / 100.0),
                        row.specular.map(|held| (held * 100.0).round() / 100.0),
                    );
                }
            }
        }
        for sampler in held.samplers() {
            let texture = sampler
                .texture_index()
                .and_then(|at| held.textures().get(usize::from(at)))
                .map_or("none", |held| held.path());
            println!("  sampler {} -> {texture}", named(sampler.id()));
        }
    }
}
