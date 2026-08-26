//! Raw dump of a `Sound` instance's undecoded geometry blob, as f32 and i32 side by side, to look
//! for plausible fields (radius, volume, falloff) before anything commits to naming them.
//!
//! `sound_geometry_dump <SoundEffectKind name> [count]`

use ironworks::file::layer::{InstanceData, LayerGroup};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

fn visit(
    group: &LayerGroup,
    kind: &str,
    out: &mut Vec<(String, Vec<u8>, [f32; 3])>,
    path: &str,
) {
    for layer in group.layers() {
        for instance in layer.instances() {
            if let InstanceData::Sound(sound) = instance.data()
                && format!("{:?}", sound.kind()) == kind
            {
                out.push((
                    format!("{path} #{}", instance.id()),
                    sound.binary().clone(),
                    instance.transform().translation(),
                ));
            }
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).expect("the path list");
    let kind = std::env::args().nth(1).unwrap_or_else(|| "Point".to_owned());
    let want: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);

    let mut found = Vec::new();
    for path in list.lines().filter(|path| path.ends_with(".lvb")) {
        if found.len() >= want {
            break;
        }
        let Ok(level) = ironworks.file::<ironworks::file::lvb::LevelFile>(path) else {
            continue;
        };
        let scene = level.scene();
        for group in scene.layer_groups() {
            visit(group, &kind, &mut found, path);
        }
        for lgb_path in scene.layer_group_paths() {
            if let Ok(file) = ironworks.file::<ironworks::file::lgb::LayerGroupFile>(lgb_path) {
                visit(file.group(), &kind, &mut found, path);
            }
        }
    }

    for (label, bytes, translation) in found.iter().take(want) {
        println!("== {label}  ({} bytes)  transform {translation:?}", bytes.len());
        for chunk in bytes.chunks(4).enumerate() {
            let (index, four) = chunk;
            if four.len() < 4 {
                continue;
            }
            let raw: [u8; 4] = four.try_into().unwrap();
            let f = f32::from_le_bytes(raw);
            let i = i32::from_le_bytes(raw);
            println!("  +{:#04x}  i32={i:<12}  f32={f:<14.4}  bytes={:02x?}", index * 4, four);
        }
    }
}
