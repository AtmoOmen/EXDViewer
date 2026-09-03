//! What motions a pack holds and how each blends, so the one a model should open on can be picked
//! by name rather than by position.
//!
//! `pap_names <path.pap> ...`

use ironworks::file::File as _;
use ironworks::file::pap::AnimationPack;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(&path) else {
            println!("{path}: absent");
            continue;
        };
        let Ok(file) = AnimationPack::read(std::io::Cursor::new(bytes)) else {
            println!("{path}: unreadable");
            continue;
        };
        let bindings = file.parse_animations().unwrap_or_default();
        println!("{path}  {} animations", file.animations().len());
        for animation in file.animations() {
            let at = usize::try_from(animation.havok_index()).unwrap_or(usize::MAX);
            let hint = bindings.get(at).map(|held| held.blend_hint());
            let span = bindings.get(at).map(|held| held.motion().duration());
            println!(
                "  {:<24} havok {at:>3}  blend {:?}  {:?}",
                animation.name(),
                hint,
                span
            );
        }
    }
}
