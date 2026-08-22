//! How many scene containers read at all, so a change to the scene reader can be checked against
//! the whole corpus rather than against the files a probe happens to touch.
//!
//! `scene_readable <sgb paths file> <lgb paths file>`

use ironworks::file::{lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let scenes = std::fs::read_to_string(args.next().expect("an sgb paths file")).unwrap();
    let levels = std::fs::read_to_string(args.next().expect("an lgb paths file")).unwrap();

    let (mut read, mut failed, mut timelines) = (0usize, 0usize, 0usize);
    for path in scenes.lines() {
        match ironworks.file::<SharedGroupFile>(path) {
            Ok(held) => {
                read += 1;
                timelines += held.scene().timelines().len();
            }
            Err(_) => failed += 1,
        }
    }
    println!("sgb: {read} read, {failed} failed, {timelines} timelines");

    let (mut read, mut failed, mut timelines) = (0usize, 0usize, 0usize);
    for path in levels.lines().filter(|path| path.ends_with(".lvb")) {
        match ironworks.file::<LevelFile>(path) {
            Ok(held) => {
                read += 1;
                timelines += held.scene().timelines().len();
            }
            Err(_) => failed += 1,
        }
    }
    println!("lvb: {read} read, {failed} failed, {timelines} timelines");
}
