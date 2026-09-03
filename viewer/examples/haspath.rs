//! Whether the install holds the paths named on the command line, and how large each is.

use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let sqpack = std::env::var("SQPACK").unwrap_or_else(|_| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    for path in std::env::args().skip(1) {
        match ironworks.file::<Vec<u8>>(&path) {
            Ok(bytes) => println!("{:>9} B  {path}", bytes.len()),
            Err(_) => println!("        -  {path}"),
        }
    }
}
