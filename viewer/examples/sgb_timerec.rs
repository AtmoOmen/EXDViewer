//! The stride of the scene timeline record, settled by which one makes every record's own fields
//! resolve: a `TMLB` region ahead of it, a string, and a list of instance pairs.
//!
//! `sgb_timerec <sgb paths file>`

use std::collections::BTreeMap;

use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let list = std::fs::read_to_string(std::env::args().nth(1).expect("an sgb paths file")).unwrap();

    let mut hits: BTreeMap<usize, usize> = BTreeMap::new();
    let mut records = 0usize;
    let mut multiple = 0usize;
    // Every byte offset inside a record, against the values it takes over the corpus.
    let mut fields: Vec<BTreeMap<u8, usize>> = vec![BTreeMap::new(); 44];
    let mut named: BTreeMap<usize, usize> = BTreeMap::new();

    for path in list.lines() {
        let Ok(bytes) = ironworks.file::<Vec<u8>>(path) else {
            continue;
        };
        // The scene body sits past the container and section headers, found by the same walk the
        // game does: the SCN1 section's body.
        let Some(body) = (0..bytes.len().saturating_sub(4))
            .find(|&at| &bytes[at..at + 4] == b"SCN1")
            .map(|at| at + 8)
        else {
            continue;
        };
        let Some(slot) = i32_at(&bytes, body + 16).filter(|held| *held > 0) else {
            continue;
        };
        let head = body + slot as usize;
        let (Some(first), Some(count)) = (i32_at(&bytes, head), i32_at(&bytes, head + 4)) else {
            continue;
        };
        if count <= 0 {
            continue;
        }
        let entries = head + first as usize;
        if count > 1 {
            multiple += 1;
        }
        for stride in [36usize, 44] {
            let mut good = 0;
            for index in 0..count as usize {
                let at = entries + index * stride;
                let Some(back) = i32_at(&bytes, at + 20) else {
                    continue;
                };
                let held = at as i64 + i64::from(back);
                if held >= 0
                    && bytes
                        .get(held as usize..held as usize + 4)
                        .is_some_and(|magic| magic == b"TMLB")
                {
                    good += 1;
                }
            }
            if good == count as usize {
                *hits.entry(stride).or_default() += 1;
            }
        }
        for index in 0..count as usize {
            let at = entries + index * 44;
            let Some(back) = i32_at(&bytes, at + 20) else {
                continue;
            };
            let held = at as i64 + i64::from(back);
            if held < 0
                || bytes
                    .get(held as usize..held as usize + 4)
                    .is_none_or(|magic| magic != b"TMLB")
            {
                continue;
            }
            records += 1;
            for (offset, byte) in bytes[at..at + 44].iter().enumerate() {
                *fields[offset].entry(*byte).or_default() += 1;
            }
            // Where a dword reads as an offset to a string inside the file.
            for offset in (0..44).step_by(4) {
                let Some(held) = i32_at(&bytes, at + offset) else {
                    continue;
                };
                let seat = at as i64 + i64::from(held);
                if held != 0
                    && seat >= 0
                    && bytes
                        .get(seat as usize)
                        .is_some_and(|byte| byte.is_ascii_graphic())
                {
                    *named.entry(offset).or_default() += 1;
                }
            }
        }
    }
    println!("scenes where every record's own TMLB resolves, by stride: {hits:?}");
    println!("{multiple} scenes hold more than one timeline; {records} records read at 44");
    for (offset, values) in fields.iter().enumerate() {
        let shown: Vec<String> = values
            .iter()
            .take(6)
            .map(|(byte, count)| format!("{byte:#04x}x{count}"))
            .collect();
        println!(
            "   +{offset:<3} {:>4} distinct  {}  string-like {}",
            values.len(),
            shown.join(" "),
            named.get(&(offset - offset % 4)).copied().unwrap_or(0)
        );
    }
}
