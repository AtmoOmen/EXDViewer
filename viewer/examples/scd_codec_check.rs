use ironworks::file::scd::SoundContainer;
use ironworks::{Ironworks, sqpack::{Install, SqPack}};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    for path in std::env::args().skip(1) {
        match ironworks.file::<SoundContainer>(&path) {
            Ok(container) => {
                for (i, entry) in container.entries().iter().enumerate() {
                    println!("{path} entry {i}: {:?} channels={} rate={} bytes={}", entry.format(), entry.channel_count(), entry.sample_rate(), entry.data().len());
                }
            }
            Err(e) => println!("{path}: {e}"),
        }
    }
}
