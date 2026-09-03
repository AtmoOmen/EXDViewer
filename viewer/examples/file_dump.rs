//! Write a file out of the install, so anything can be run over its bytes.
//!
//! `file_dump <path> <out>`

use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a path");
    let out = args.next().expect("somewhere to write it");
    let bytes = ironworks.file::<Vec<u8>>(&path).expect("the file");
    std::fs::write(&out, &bytes).expect("the write");
    println!("{path}: {} bytes -> {out}", bytes.len());
}
