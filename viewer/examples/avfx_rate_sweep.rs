//! Scratch tool: histogram emitter `Life` and timeline `EdTm` frame counts across the avfx corpus,
//! to see whether they cluster at multiples of 30 or 60 (evidence for the authoring frame rate).

use std::collections::BTreeMap;
use std::io::Cursor;

use ironworks::{
    Ironworks,
    file::File,
    file::avfx::Avfx,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let sqpack = std::env::var("SQPACK").unwrap_or_else(|_| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    let list_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/asriel/Code/ironworks-formats/paths.txt".to_owned());
    let paths: Vec<String> = std::fs::read_to_string(&list_path)
        .unwrap()
        .lines()
        .filter(|line| line.ends_with(".avfx"))
        .map(str::to_owned)
        .collect();

    let mut life_mod30 = 0u64;
    let mut life_mod60_not30 = 0u64;
    let mut life_other = 0u64;
    let mut edtm_mod30 = 0u64;
    let mut edtm_mod60_not30 = 0u64;
    let mut edtm_other = 0u64;
    let mut life_hist: BTreeMap<i64, u64> = BTreeMap::new();
    let mut edtm_hist: BTreeMap<i64, u64> = BTreeMap::new();
    let mut files_ok = 0u64;
    let mut files_err = 0u64;

    for (i, path) in paths.iter().enumerate() {
        if i % 5000 == 0 {
            eprintln!("{i}/{}", paths.len());
        }
        let bytes: Vec<u8> = match ironworks.file(path) {
            Ok(b) => b,
            Err(_) => {
                files_err += 1;
                continue;
            }
        };
        let file = match Avfx::read(Cursor::new(bytes)) {
            Ok(f) => f,
            Err(_) => {
                files_err += 1;
                continue;
            }
        };
        files_ok += 1;

        for emitter in file.emitters() {
            if let Some(life) = emitter
                .properties()
                .iter()
                .find(|b| b.name() == "Life")
                .and_then(|b| b.find("Val"))
                .and_then(|b| b.f32())
            {
                if life >= 0.0 && life.fract() == 0.0 && life < 100000.0 {
                    let v = life as i64;
                    *life_hist.entry(v).or_default() += 1;
                    if v != 0 && v % 30 == 0 {
                        life_mod30 += 1;
                    } else if v != 0 && v % 60 == 0 {
                        life_mod60_not30 += 1;
                    } else {
                        life_other += 1;
                    }
                }
            }
        }

        for timeline in file.timelines() {
            for item in timeline.items() {
                if let Some(edtm) = item.find("EdTm").and_then(|b| b.i32()) {
                    if edtm > 0 && edtm < 100000 {
                        let v = edtm as i64;
                        *edtm_hist.entry(v).or_default() += 1;
                        if v % 30 == 0 {
                            edtm_mod30 += 1;
                        } else if v % 60 == 0 {
                            edtm_mod60_not30 += 1;
                        } else {
                            edtm_other += 1;
                        }
                    }
                }
            }
        }
    }

    println!("files ok={files_ok} err={files_err}");
    println!(
        "Life:  %30==0: {life_mod30}  %60==0-and-not-%30: {life_mod60_not30}  other: {life_other}"
    );
    println!(
        "EdTm:  %30==0: {edtm_mod30}  %60==0-and-not-%30: {edtm_mod60_not30}  other: {edtm_other}"
    );

    println!("\ntop 30 Life values:");
    let mut life_sorted: Vec<_> = life_hist.into_iter().collect();
    life_sorted.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    for (v, c) in life_sorted.iter().take(30) {
        println!("  {v:6} frames  x{c}");
    }

    println!("\ntop 30 EdTm values:");
    let mut edtm_sorted: Vec<_> = edtm_hist.into_iter().collect();
    edtm_sorted.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    for (v, c) in edtm_sorted.iter().take(30) {
        println!("  {v:6} frames  x{c}");
    }
}
