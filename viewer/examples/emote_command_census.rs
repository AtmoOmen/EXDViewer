//! What an emote's own timeline commands carry: which `Cxxx` kinds appear, what their path
//! fields point at (extension), and samples of the ones that carry props, vfx or sound.
//!
//! `emote_command_census <paths file>`

use std::collections::BTreeMap;
use std::path::Path;

use ironworks::file::File;
use ironworks::file::pap::AnimationPack;
use ironworks::file::tmb::{CommandKind, Item, Timeline};
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

#[derive(Default)]
struct Census {
    read: usize,
    failed: usize,
    timelines: usize,
    /// How many commands of each magic, across every timeline.
    kinds: BTreeMap<&'static str, usize>,
    /// For a path-carrying kind, how many paths end in each extension.
    extensions: BTreeMap<&'static str, BTreeMap<String, usize>>,
    /// One example path per (kind, extension).
    examples: BTreeMap<(&'static str, String), String>,
    /// C043 weapon/body/variant triples seen, with an owning file.
    c043: Vec<(i16, i16, i32, String)>,
    /// C198 model/body/variant/summon_id/atch_state, with an owning file.
    c198: Vec<(i16, i16, i32, u8, u8, String)>,
    /// C107 VFXTrigger rows seen, with an owning file.
    c107: Vec<(i32, String)>,
    /// C012 bind fields, tallied by (bind_origin_1, bind_type_1).
    c012_bind: BTreeMap<(u8, u8), usize>,
    /// C012 position vector lengths seen.
    c012_position_len: BTreeMap<usize, usize>,
    /// C012 bind_id_1 tallies.
    c012_bind_id: BTreeMap<i16, usize>,
    /// C063 bind fields.
    c063_bind: BTreeMap<u8, usize>,
}

fn kind_name(kind: &CommandKind) -> &'static str {
    match kind {
        CommandKind::C002(_) => "C002",
        CommandKind::C004(_) => "C004",
        CommandKind::C006(_) => "C006",
        CommandKind::C009(_) => "C009",
        CommandKind::C010(_) => "C010",
        CommandKind::C011(_) => "C011",
        CommandKind::C012(_) => "C012",
        CommandKind::C013(_) => "C013",
        CommandKind::C014(_) => "C014",
        CommandKind::C015(_) => "C015",
        CommandKind::C018(_) => "C018",
        CommandKind::C019(_) => "C019",
        CommandKind::C021(_) => "C021",
        CommandKind::C031(_) => "C031",
        CommandKind::C033(_) => "C033",
        CommandKind::C034(_) => "C034",
        CommandKind::C040(_) => "C040",
        CommandKind::C042(_) => "C042",
        CommandKind::C043(_) => "C043",
        CommandKind::C048(_) => "C048",
        CommandKind::C053(_) => "C053",
        CommandKind::C055(_) => "C055",
        CommandKind::C056(_) => "C056",
        CommandKind::C057(_) => "C057",
        CommandKind::C058(_) => "C058",
        CommandKind::C059(_) => "C059",
        CommandKind::C063(_) => "C063",
        CommandKind::C067(_) => "C067",
        CommandKind::C068(_) => "C068",
        CommandKind::C075(_) => "C075",
        CommandKind::C082(_) => "C082",
        CommandKind::C083(_) => "C083",
        CommandKind::C084(_) => "C084",
        CommandKind::C088(_) => "C088",
        CommandKind::C089(_) => "C089",
        CommandKind::C090(_) => "C090",
        CommandKind::C093(_) => "C093",
        CommandKind::C094(_) => "C094",
        CommandKind::C095(_) => "C095",
        CommandKind::C100(_) => "C100",
        CommandKind::C104(_) => "C104",
        CommandKind::C107(_) => "C107",
        CommandKind::C109(_) => "C109",
        CommandKind::C110(_) => "C110",
        CommandKind::C112(_) => "C112",
        CommandKind::C113(_) => "C113",
        CommandKind::C117(_) => "C117",
        CommandKind::C118(_) => "C118",
        CommandKind::C120(_) => "C120",
        CommandKind::C124(_) => "C124",
        CommandKind::C125(_) => "C125",
        CommandKind::C131(_) => "C131",
        CommandKind::C133(_) => "C133",
        CommandKind::C136(_) => "C136",
        CommandKind::C139(_) => "C139",
        CommandKind::C142(_) => "C142",
        CommandKind::C143(_) => "C143",
        CommandKind::C144(_) => "C144",
        CommandKind::C161(_) => "C161",
        CommandKind::C168(_) => "C168",
        CommandKind::C173(_) => "C173",
        CommandKind::C174(_) => "C174",
        CommandKind::C175(_) => "C175",
        CommandKind::C176(_) => "C176",
        CommandKind::C177(_) => "C177",
        CommandKind::C178(_) => "C178",
        CommandKind::C187(_) => "C187",
        CommandKind::C188(_) => "C188",
        CommandKind::C192(_) => "C192",
        CommandKind::C194(_) => "C194",
        CommandKind::C197(_) => "C197",
        CommandKind::C198(_) => "C198",
        CommandKind::C199(_) => "C199",
        CommandKind::C202(_) => "C202",
        CommandKind::C203(_) => "C203",
        CommandKind::C204(_) => "C204",
        CommandKind::C211(_) => "C211",
        CommandKind::C212(_) => "C212",
        CommandKind::C215(_) => "C215",
        CommandKind::C216(_) => "C216",
        CommandKind::C225(_) => "C225",
        CommandKind::C230(_) => "C230",
        CommandKind::C234(_) => "C234",
        CommandKind::Unknown { .. } => "Unknown",
    }
}

fn ext(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(none)".to_owned())
}

fn walk(census: &mut Census, path: &str, timeline: &[u8]) {
    let Ok(parsed) = Timeline::read(std::io::Cursor::new(timeline.to_vec())) else {
        return;
    };
    census.timelines += 1;
    for item in parsed.items() {
        let Item::Command(command) = item else {
            continue;
        };
        let name = kind_name(command.kind());
        *census.kinds.entry(name).or_default() += 1;
        match command.kind() {
            CommandKind::C002(c) => note_path(census, name, c.path(), path),
            CommandKind::C009(c) => note_path(census, name, c.motion(), path),
            CommandKind::C010(c) => note_path(census, name, c.motion(), path),
            CommandKind::C012(c) => {
                note_path(census, name, c.path(), path);
                *census
                    .c012_bind
                    .entry((c.bind_origin_1(), c.bind_type_1()))
                    .or_default() += 1;
                *census
                    .c012_position_len
                    .entry(c.position().len())
                    .or_default() += 1;
                *census.c012_bind_id.entry(c.bind_id_1()).or_default() += 1;
            }
            CommandKind::C063(c) => {
                note_path(census, name, c.path(), path);
                *census.c063_bind.entry(c.bind_id()).or_default() += 1;
            }
            CommandKind::C173(c) => note_path(census, name, c.path(), path),
            CommandKind::C043(c) => {
                if census.c043.len() < 20 {
                    census
                        .c043
                        .push((c.weapon_id(), c.body_id(), c.variant_id(), path.to_owned()));
                }
            }
            CommandKind::C198(c) => {
                if census.c198.len() < 40 {
                    census.c198.push((
                        c.model_id(),
                        c.body_id(),
                        c.variant(),
                        c.summon_id(),
                        c.atch_state(),
                        path.to_owned(),
                    ));
                }
            }
            CommandKind::C107(c) => {
                if census.c107.len() < 20 {
                    census.c107.push((c.trigger_row(), path.to_owned()));
                }
            }
            _ => {}
        }
    }
}

fn note_path(census: &mut Census, kind: &'static str, path: Option<&str>, file: &str) {
    let Some(path) = path else { return };
    let extension = ext(path);
    *census
        .extensions
        .entry(kind)
        .or_default()
        .entry(extension.clone())
        .or_default() += 1;
    census
        .examples
        .entry((kind, extension))
        .or_insert_with(|| format!("{path} (in {file})"));
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::env::args().nth(1).expect("a paths file");
    let paths = std::fs::read_to_string(list).expect("the paths file");

    let mut census = Census::default();
    for path in paths.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let bytes = match ironworks.file::<Vec<u8>>(path) {
            Ok(bytes) => bytes,
            Err(_) => {
                census.failed += 1;
                continue;
            }
        };
        let Ok(pack) = AnimationPack::read(std::io::Cursor::new(bytes)) else {
            census.failed += 1;
            continue;
        };
        census.read += 1;
        for timeline in pack.timelines() {
            walk(&mut census, path, timeline);
        }
    }

    println!(
        "read {} / {} packs, {} failed, {} timelines",
        census.read,
        census.read + census.failed,
        census.failed,
        census.timelines
    );
    println!("\ncommand kinds:");
    for (kind, count) in &census.kinds {
        println!("  {kind}: {count}");
    }
    println!("\npath extensions per kind:");
    for (kind, exts) in &census.extensions {
        for (extension, count) in exts {
            let example = census
                .examples
                .get(&(*kind, extension.clone()))
                .cloned()
                .unwrap_or_default();
            println!("  {kind} .{extension}: {count} (e.g. {example})");
        }
    }
    println!("\nC012 bind (origin, type) tallies:");
    for ((origin, kind), count) in &census.c012_bind {
        println!("  origin {origin} type {kind}: {count}");
    }
    println!("\nC012 position vector lengths:");
    for (len, count) in &census.c012_position_len {
        println!("  {len}: {count}");
    }
    println!("\nC012 bind_id_1 tallies:");
    for (id, count) in &census.c012_bind_id {
        println!("  {id}: {count}");
    }
    println!("\nC063 bind_id tallies:");
    for (bind, count) in &census.c063_bind {
        println!("  {bind}: {count}");
    }
    println!("\nC043 (weapon_id, body_id, variant_id) samples:");
    for (weapon, body, variant, file) in &census.c043 {
        println!("  w{weapon:04}b{body:04} variant {variant} in {file}");
    }
    println!("\nC198 (model_id, body_id, variant, summon_id, atch_state) samples:");
    for (model, body, variant, summon, atch, file) in &census.c198 {
        println!(
            "  model {model} body {body} variant {variant} summon {summon} atch {atch} in {file}"
        );
    }
    println!("\nC107 VFXTrigger row samples:");
    for (row, file) in &census.c107 {
        println!("  row {row} in {file}");
    }
}
