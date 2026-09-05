//! The seven timelines each emote names, by slot, with the `ActionTimeline` fields that say what
//! part of the body one drives, and every key the sheet holds.
//!
//! `emote_timelines [--columns] [--paths] [--keys <substring>]`

use std::collections::BTreeSet;
use std::sync::Arc;

use ironworks::excel::{Excel, Field, Language, Row};
use ironworks::file::exh::ColumnDefinition;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

/// `Emote`'s name and the seven timelines it names, as byte offsets.
const NAME: u16 = 0;
const TIMELINES: [u16; 7] = [16, 18, 20, 22, 24, 26, 28];
/// `ActionTimeline`'s key, and the fields that say what a motion drives: sorted by byte offset,
/// its columns line up one for one with EXDSchema's own field order.
const KEY: u16 = 0;
const KILL_UPPER: u16 = 6;
const PRIORITY: u16 = 10;
const STANCE: u16 = 11;
const SLOT: u16 = 12;

fn column(columns: &[ColumnDefinition], offset: u16) -> ColumnDefinition {
    columns
        .iter()
        .find(|column| column.offset() == offset)
        .cloned()
        .unwrap_or_else(|| panic!("no column at offset {offset}"))
}

fn int(row: &Row, column: &ColumnDefinition) -> i64 {
    match row.field(column) {
        Ok(Field::U8(v)) => i64::from(v),
        Ok(Field::I8(v)) => i64::from(v),
        Ok(Field::U16(v)) => i64::from(v),
        Ok(Field::I16(v)) => i64::from(v),
        Ok(Field::U32(v)) => i64::from(v),
        Ok(Field::I32(v)) => i64::from(v),
        Ok(Field::Bool(v)) => i64::from(v),
        _ => 0,
    }
}

fn text(row: &Row, column: &ColumnDefinition) -> String {
    match row.field(column) {
        Ok(Field::String(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn main() {
    let ironworks: Arc<Ironworks> = Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks).with_default_language(Language::English);
    let emotes = excel.sheet("Emote").expect("Emote");
    let timelines = excel.sheet("ActionTimeline").expect("ActionTimeline");
    let emote_columns = emotes.columns().expect("Emote columns");
    let timeline_columns = timelines.columns().expect("ActionTimeline columns");

    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args.iter().position(|arg| arg == "--keys") {
        let wanted = args.get(at + 1).cloned().unwrap_or_default();
        let key_column = column(&timeline_columns, KEY);
        for row in timelines.into_iter() {
            let key = text(&row, &key_column);
            if !key.is_empty() && key.contains(&wanted) {
                println!("{key}");
            }
        }
        return;
    }
    if args.iter().any(|arg| arg == "--columns") {
        let mut sorted = timeline_columns.clone();
        sorted.sort_by_key(|column| (column.offset(), format!("{:?}", column.kind())));
        for column in &sorted {
            println!("ActionTimeline +{:<4} {:?}", column.offset(), column.kind());
        }
        let mut sorted = emote_columns.clone();
        sorted.sort_by_key(|column| (column.offset(), format!("{:?}", column.kind())));
        for column in &sorted {
            println!("Emote +{:<4} {:?}", column.offset(), column.kind());
        }
        return;
    }

    let key_column = column(&timeline_columns, KEY);
    let name_column = column(&emote_columns, NAME);
    let slot_columns: Vec<ColumnDefinition> =
        TIMELINES.iter().map(|at| column(&emote_columns, *at)).collect();
    let others: Vec<(&str, ColumnDefinition)> = vec![
        ("kill", column(&timeline_columns, KILL_UPPER)),
        ("pri", column(&timeline_columns, PRIORITY)),
        ("stance", column(&timeline_columns, STANCE)),
        ("slot", column(&timeline_columns, SLOT)),
    ];

    let paths_only = args.iter().any(|arg| arg == "--paths");
    let mut keys = BTreeSet::new();
    for row in emotes.into_iter() {
        let name = text(&row, &name_column);
        if name.is_empty() {
            continue;
        }
        let mut slots = Vec::new();
        for held in &slot_columns {
            let id = int(&row, held);
            if id <= 0 {
                slots.push("-".to_owned());
                continue;
            }
            let Ok(timeline) = timelines.row(id as u32) else {
                slots.push(format!("#{id}"));
                continue;
            };
            let key = text(&timeline, &key_column);
            keys.insert(key.clone());
            let fields: Vec<String> = others
                .iter()
                .map(|(name, held)| format!("{name}{}", int(&timeline, held)))
                .collect();
            slots.push(format!("{key}({})", fields.join(" ")));
        }
        if !paths_only {
            println!("{name}\t{}", slots.join("\t"));
        }
    }
    if paths_only {
        for key in keys {
            println!("chara/human/c0101/animation/a0001/bt_common/{key}.pap");
        }
    }
}
