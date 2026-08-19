//! How many shaders of each stage a package ships, which is what says whether a pass needs a stage
//! the browser has no equivalent of.

use ironworks::file::shpk::{ShaderPackage, Stage};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

const PACKAGES: [&str; 10] = [
    "bg",
    "bgcolorchange",
    "bgcrestchange",
    "bguvscroll",
    "character",
    "crystal",
    "water",
    "river",
    "cloud",
    "verticalfog",
];

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let asked: Vec<String> = std::env::args().skip(1).collect();
    let wanted = match asked.is_empty() {
        true => PACKAGES.iter().map(|held| (*held).to_owned()).collect(),
        false => asked,
    };
    let mut tessellated = 0usize;
    for name in &wanted {
        let path = format!("shader/sm5/shpk/{name}.shpk");
        let Ok(package) = ironworks.file::<ShaderPackage>(&path) else {
            println!("{name:<16} not present");
            continue;
        };
        let mut counts = [0usize; 6];
        for shader in package.shaders() {
            let at = match shader.stage() {
                Stage::Vertex => 0,
                Stage::Pixel => 1,
                Stage::Geometry => 2,
                Stage::Hull => 3,
                Stage::Domain => 4,
                _ => 5,
            };
            counts[at] += 1;
        }
        tessellated += counts[3] + counts[4];
        println!(
            "{name:<16} {:>5} vertex  {:>5} pixel  {:>3} geometry  {:>3} hull  {:>3} domain  {:>3} other",
            counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
        );
    }
    println!(
        "\n{tessellated} hull or domain shaders across {} packages",
        wanted.len()
    );
}
