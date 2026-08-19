//! What a zone places around one point in the world, and which shared group each of it came under.
//!
//! `zone_near <path.lvb> <x> <y> <z> [radius]`

use ironworks::file::layer::InstanceData;
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

use glam::{Mat4, Quat, Vec3};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const DEPTH: usize = 6;

type Pack = Ironworks<SqPack<Install>>;

fn matrix(placed: ironworks::file::layer::Transform) -> Mat4 {
    let rotation = placed.rotation();
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(placed.scale()),
        Quat::from_euler(glam::EulerRot::ZYX, rotation[2], rotation[1], rotation[0]),
        Vec3::from_array(placed.translation()),
    )
}

fn walk(
    ironworks: &Pack,
    instances: &[ironworks::file::layer::Instance],
    transform: Mat4,
    under: &str,
    at: Vec3,
    reach: f32,
    depth: usize,
) {
    for instance in instances {
        let here = transform * matrix(instance.transform());
        let center = here.transform_point3(Vec3::ZERO);
        let near = center.distance(at) <= reach;
        match instance.data() {
            InstanceData::BgPart(held) if near && !held.asset_path().is_empty() => println!(
                "  {:6.1}  bgpart  {}  under {under}",
                center.distance(at),
                held.asset_path()
            ),
            InstanceData::Vfx(held) if near => println!(
                "  {:6.1}  vfx     {}  under {under}",
                center.distance(at),
                held.asset_path()
            ),
            InstanceData::Aetheryte(_) if near => {
                println!("  {:6.1}  aetheryte marker  under {under}", center.distance(at))
            }
            InstanceData::SharedGroup(held) if depth < DEPTH && !held.asset_path().is_empty() => {
                let Ok(file) = ironworks.file::<SharedGroupFile>(held.asset_path()) else {
                    continue;
                };
                let scene = file.scene();
                if near {
                    println!(
                        "  {:6.1}  group   {}  {} timelines  under {under}",
                        center.distance(at),
                        held.asset_path(),
                        scene.timelines().len()
                    );
                }
                for group in scene.layer_groups() {
                    for layer in group.layers() {
                        walk(
                            ironworks,
                            layer.instances(),
                            here,
                            held.asset_path(),
                            at,
                            reach,
                            depth + 1,
                        );
                    }
                }
            }
            _ => (),
        }
    }
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let mut args = std::env::args().skip(1);
    let zone = args.next().expect("a level path");
    let number = |held: Option<String>| held.and_then(|one| one.parse().ok());
    let at = Vec3::new(
        number(args.next()).expect("x"),
        number(args.next()).expect("y"),
        number(args.next()).expect("z"),
    );
    let reach = number(args.next()).unwrap_or(30.0);

    let level: LevelFile = ironworks.file(&zone).unwrap();
    for path in level.scene().layer_group_paths() {
        let Ok(file) = ironworks.file::<LayerGroupFile>(path) else {
            continue;
        };
        println!("== {path}");
        for layer in file.group().layers() {
            walk(
                &ironworks,
                layer.instances(),
                Mat4::IDENTITY,
                layer.name(),
                at,
                reach,
                0,
            );
        }
    }
}
