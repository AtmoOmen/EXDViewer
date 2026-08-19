//! The shader parameter rows, by name, for the profiles a family holds.

use std::io::Cursor;

use ironworks::Ironworks;
use ironworks::file::spm::{self, ShaderParameters};
use ironworks::file::File;
use ironworks::sqpack::{Install, SqPack};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let bytes: Vec<u8> = ironworks.file(&path).expect("the file");
        let file = ShaderParameters::read(Cursor::new(bytes)).expect("a parameter file");
        let names: Vec<String> = file
            .columns()
            .iter()
            .map(|held| {
                spm::name(held.id())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{:08x}", held.id()))
            })
            .collect();
        println!("== {path}  {} rows", file.rows().len());
        for profile in 0..file.rows().len() {
            let held: Vec<String> = names
                .iter()
                .enumerate()
                .filter_map(|(column, name)| {
                    let shown = match file.value(profile, column)? {
                        spm::Value::Float(held) if held == 0.0 => return None,
                        spm::Value::Float(held) => format!("{held}"),
                        spm::Value::Unsigned(held) if held == 0 => return None,
                        spm::Value::Unsigned(held) => format!("{held}"),
                        held => format!("{held:?}"),
                    };
                    Some(format!("{name}={shown}"))
                })
                .collect();
            println!("  [{profile:3}] {}", held.join(" "));
        }
    }
}
