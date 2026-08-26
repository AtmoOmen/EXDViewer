use ironworks::file::layer::InstanceData;
use ironworks::{Ironworks, sqpack::{Install, SqPack}};
use std::collections::BTreeMap;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).unwrap();
    let mut empty_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut nonempty_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for path in list.lines().filter(|p| p.ends_with(".lvb")) {
        let Ok(level) = ironworks.file::<ironworks::file::lvb::LevelFile>(path) else { continue };
        let scene = level.scene();
        for lgb_path in scene.layer_group_paths() {
            let Ok(file) = ironworks.file::<ironworks::file::lgb::LayerGroupFile>(lgb_path) else { continue };
            for layer in file.group().layers() {
                for instance in layer.instances() {
                    if let InstanceData::Sound(sound) = instance.data() {
                        let kind = format!("{:?}", sound.kind());
                        if sound.asset_path().is_empty() {
                            *empty_by_kind.entry(kind).or_default() += 1;
                        } else {
                            *nonempty_by_kind.entry(kind).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    println!("empty asset_path by kind: {empty_by_kind:?}");
    println!("nonempty asset_path by kind: {nonempty_by_kind:?}");
}
