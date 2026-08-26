//! What a zone states about its sounds: `.essb` references from `.lvb` scenes, and the `Sound`
//! instances a zone's layer groups place.
//!
//! `zone_sound_census`

use std::collections::{BTreeMap, HashMap, HashSet};

use ironworks::file::layer::{InstanceData, LayerGroup};
use ironworks::file::scd::{Codec, SoundContainer};
use ironworks::file::{essb, lgb, lvb};
use ironworks::{
    Ironworks, Resource,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";

/// The env kind that names ambient sound assets. See `ironworks::file::envs`.
const AMBIENT_SOUND_PATHS_KIND: u32 = 20;

fn visit(
    group: &LayerGroup,
    tally: &mut Tally,
    ironworks: &Ironworks<impl Resource>,
    lvb_path: &str,
    essb_refs: &mut HashMap<String, HashSet<String>>,
    embedded: bool,
) {
    for layer in group.layers() {
        for instance in layer.instances() {
            if let InstanceData::EnvSpace(space) = instance.data() {
                tally.env_spaces += 1;
                *tally
                    .env_space_shape
                    .entry(format!("{:?}", space.shape()))
                    .or_default() += 1;
                if !space.sound_asset_path().is_empty() {
                    essb_refs
                        .entry(space.sound_asset_path().clone())
                        .or_default()
                        .insert(lvb_path.to_owned());
                }
            }
            if let InstanceData::Sound(sound) = instance.data() {
                tally.sounds += 1;
                match embedded {
                    true => tally.sounds_embedded += 1,
                    false => tally.sounds_external += 1,
                }
                let kind = format!("{:?}", sound.kind());
                *tally.by_kind.entry(kind.clone()).or_default() += 1;
                if sound.auto_play() {
                    tally.auto_play += 1;
                }
                if sound.no_far_clip() {
                    tally.no_far_clip += 1;
                }
                tally
                    .binary_len_by_kind
                    .entry(kind)
                    .or_default()
                    .insert(sound.binary().len());
                tally.point_selection.insert(sound.point_selection());

                let path = sound.asset_path().clone();
                if !path.is_empty() && tally.scd_checked.insert(path.clone()) {
                    match ironworks.file::<SoundContainer>(&path) {
                        Ok(container) => {
                            tally.scd_ok += 1;
                            let first_ogg = container
                                .entries()
                                .first()
                                .is_some_and(|entry| matches!(entry.format(), Codec::OggVorbis));
                            if first_ogg && tally.ogg_examples.len() < 8 {
                                tally.ogg_examples.push(format!("{lvb_path}  {path}"));
                            }
                            for entry in container.entries() {
                                *tally.codec.entry(format!("{:?}", entry.format())).or_default() +=
                                    1;
                            }
                        }
                        Err(_) => tally.scd_missing += 1,
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct Tally {
    sounds: usize,
    sounds_embedded: usize,
    sounds_external: usize,
    by_kind: BTreeMap<String, usize>,
    auto_play: usize,
    no_far_clip: usize,
    binary_len_by_kind: BTreeMap<String, HashSet<usize>>,
    point_selection: HashSet<u32>,
    scd_checked: HashSet<String>,
    scd_ok: usize,
    scd_missing: usize,
    codec: BTreeMap<String, usize>,
    ogg_examples: Vec<String>,
    env_spaces: usize,
    env_space_shape: BTreeMap<String, usize>,
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(PATHS).expect("the path list");

    let lvb_paths: Vec<&str> = list.lines().filter(|path| path.ends_with(".lvb")).collect();
    println!("{} .lvb paths in the list", lvb_paths.len());

    // A scene names its ambient bed's `.essb` once for the whole zone (`environments()`); an
    // `EnvSpace` instance names a second, region-local one. Kept apart: summing them conflates two
    // different altitudes of "referenced by".
    let mut essb_scene_refs: HashMap<String, HashSet<String>> = HashMap::new();
    let mut essb_envspace_refs: HashMap<String, HashSet<String>> = HashMap::new();
    let mut lvb_ok = 0;
    let mut lvb_absent = 0;
    let mut lvb_parse_error = 0;
    let mut tally = Tally::default();

    for &path in &lvb_paths {
        let level = match ironworks.file::<lvb::LevelFile>(path) {
            Ok(level) => {
                lvb_ok += 1;
                level
            }
            Err(_) => {
                match ironworks.file::<Vec<u8>>(path) {
                    Ok(_) => lvb_parse_error += 1,
                    Err(_) => lvb_absent += 1,
                }
                continue;
            }
        };
        let scene = level.scene();
        for environment in scene.environments() {
            let sound_path = environment.sound_asset_path();
            if !sound_path.is_empty() {
                essb_scene_refs
                    .entry(sound_path.clone())
                    .or_default()
                    .insert(path.to_owned());
            }
        }

        for group in scene.layer_groups() {
            visit(group, &mut tally, &ironworks, path, &mut essb_envspace_refs, true);
        }
        for lgb_path in scene.layer_group_paths() {
            if let Ok(file) = ironworks.file::<lgb::LayerGroupFile>(lgb_path) {
                visit(file.group(), &mut tally, &ironworks, path, &mut essb_envspace_refs, false);
            }
        }
    }

    println!("lvb parsed ok {lvb_ok}, absent from install {lvb_absent}, parse error on real bytes {lvb_parse_error}");
    for (label, refs) in [
        ("a scene's own environment list", &essb_scene_refs),
        ("an EnvSpace instance", &essb_envspace_refs),
    ] {
        let shared = refs.values().filter(|lvbs| lvbs.len() > 1).count();
        println!(
            "essb named by {label}: {} distinct, {shared} named by more than one lvb",
            refs.len()
        );
        for (path, lvbs) in refs.iter().filter(|(_, lvbs)| lvbs.len() > 1) {
            println!("  {path}  x{}", lvbs.len());
        }
    }

    let essb_paths: Vec<&str> = list.lines().filter(|path| path.ends_with(".essb")).collect();
    let mut essb_ok = 0;
    let mut essb_absent = 0;
    let mut essb_parse_error = 0;
    let mut ambient_scd: HashSet<String> = HashSet::new();
    for path in &essb_paths {
        match ironworks.file::<essb::SoundEnvironmentFile>(path) {
            Ok(file) => {
                essb_ok += 1;
                for weather in file.environments().weathers() {
                    for set in weather.sets() {
                        if set.kind() != AMBIENT_SOUND_PATHS_KIND {
                            continue;
                        }
                        for keyframe in set.keyframes() {
                            ambient_scd.extend(
                                keyframe.paths().filter(|path| !path.is_empty()).map(str::to_owned),
                            );
                        }
                    }
                }
            }
            Err(_) => match ironworks.file::<Vec<u8>>(path) {
                Ok(_) => essb_parse_error += 1,
                Err(_) => essb_absent += 1,
            },
        }
    }
    println!(
        "\n.essb total in path list {}, parse ok {essb_ok}, absent from install {essb_absent}, parse error on real bytes {essb_parse_error}",
        essb_paths.len()
    );

    println!(
        "\ndistinct .scd named by an essb's Ambient sound paths set: {}",
        ambient_scd.len()
    );
    let mut ambient_ok = 0;
    let mut ambient_missing = 0;
    let mut ambient_codec: BTreeMap<String, usize> = BTreeMap::new();
    for path in &ambient_scd {
        match ironworks.file::<SoundContainer>(path) {
            Ok(container) => {
                ambient_ok += 1;
                for entry in container.entries() {
                    *ambient_codec.entry(format!("{:?}", entry.format())).or_default() += 1;
                }
            }
            Err(_) => ambient_missing += 1,
        }
    }
    println!("  resolved: {ambient_ok}  missing: {ambient_missing}");
    println!("  codec of every stream inside those .scd:");
    for (codec, count) in &ambient_codec {
        println!("    {codec}: {count}");
    }

    println!("\nEnvSpace instances placed: {}", tally.env_spaces);
    for (shape, count) in &tally.env_space_shape {
        println!("  {shape}: {count}");
    }

    println!(
        "\nSound instances placed: {}  (embedded in the scene {}, in an externally named .lgb {})",
        tally.sounds, tally.sounds_embedded, tally.sounds_external
    );
    println!("  auto play: {}", tally.auto_play);
    println!("  no far clip: {}", tally.no_far_clip);
    println!("  distinct point_selection values: {:?}", {
        let mut v: Vec<_> = tally.point_selection.iter().copied().collect();
        v.sort_unstable();
        v
    });
    println!("  by kind:");
    for (kind, count) in &tally.by_kind {
        let lens = &tally.binary_len_by_kind[kind];
        let mut lens: Vec<_> = lens.iter().copied().collect();
        lens.sort_unstable();
        println!("    {kind}: {count}  geometry byte lengths seen: {lens:?}");
    }

    println!(
        "\ndistinct .scd named by a Sound instance: {}",
        tally.scd_checked.len()
    );
    println!("  resolved: {}  missing: {}", tally.scd_ok, tally.scd_missing);
    println!("  codec of every stream inside those .scd:");
    for (codec, count) in &tally.codec {
        println!("    {codec}: {count}");
    }

    println!("\nzone + .scd pairs whose first stream is Ogg Vorbis, for playback verification:");
    for example in &tally.ogg_examples {
        println!("  {example}");
    }
}
