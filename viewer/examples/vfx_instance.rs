use ironworks::file::layer::InstanceData;
use ironworks::file::lgb::LayerGroupFile;
use ironworks::{Ironworks, sqpack::{Install, SqPack}};

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack("/home/asriel/.xlcore/ffxiv/game/sqpack")));
    let path = std::env::args().nth(1).expect("path");
    let file: LayerGroupFile = ironworks.file(&path).unwrap();
    for layer in file.group().layers() {
        for instance in layer.instances() {
            if let InstanceData::Vfx(vfx) = instance.data() {
                let t = instance.transform();
                println!(
                    "{}: pos={:?} rot={:?} scale={:?} auto_play={} colour={:?} fade_near={:?} fade_far={:?} no_far_clip={}",
                    vfx.asset_path(), t.translation(), t.rotation(), t.scale(),
                    vfx.auto_play(), vfx.colour(), vfx.fade_near(), vfx.fade_far(), vfx.no_far_clip(),
                );
            }
        }
    }
}
