//! throwaway
use std::io::Cursor;
use ironworks::file::File;
use ironworks::file::pap::AnimationPack;
use ironworks::file::tmb::{CommandKind, Item, Timeline};
use ironworks::{Ironworks, sqpack::{Install, SqPack}};
const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
fn main() {
    let iw = Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK))));
    for m in ["sp53", "sp53_no_target", "sp04", "sp04_no_target", "sp51", "sp34"] {
        let p = format!("chara/human/c0101/animation/a0001/bt_common/emote_sp/{m}.pap");
        let Ok(b) = iw.file::<Vec<u8>>(&p) else { println!("MISS {m}"); continue };
        let pack = AnimationPack::read(Cursor::new(b)).unwrap();
        println!("== {m}");
        for blob in pack.timelines() {
            let Ok(t) = Timeline::read(Cursor::new(blob.to_vec())) else { continue };
            for item in t.items() {
                let Item::Command(c) = item else { continue };
                match c.kind() {
                    CommandKind::C012(v) => println!("   C012 t{:4} bind1({},{},{}) bind2({},{},{}) vis{} {:?}",
                        c.time(), v.bind_origin_1(), v.bind_type_1(), v.bind_id_1(),
                        v.bind_origin_2(), v.bind_type_2(), v.bind_id_2(), v.visibility(), v.path()),
                    CommandKind::C173(v) => println!("   C173 t{:4} {:?}", c.time(), v.path()),
                    _ => {}
                }
            }
        }
    }
}
