//! What ships under `cut/`, and which sheet claims each cutscene.
//!
//! `cut_census <path list> <EXDSchema directory>`

use std::collections::{BTreeMap, HashMap, HashSet};

use ironworks::{
    Ironworks,
    excel::{Excel, Field, Language},
    file::exh::ColumnDefinition,
    sqpack::{Install, SqPack},
};
use serde::Deserialize;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const CUT: u8 = 3;
const PARAMS: usize = 50;
const JOURNAL_SLOTS: usize = 24;
const FIRST_QUEST: u32 = 65536;

#[derive(Deserialize)]
struct SchemaFile {
    fields: Vec<SchemaField>,
}

#[derive(Deserialize)]
struct SchemaField {
    name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    count: Option<usize>,
    fields: Option<Vec<SchemaField>>,
}

fn flatten(fields: &[SchemaField], scope: &str, in_array: bool, out: &mut Vec<String>) {
    for field in fields {
        let mut scope = scope.to_owned();
        match (&field.name, in_array) {
            (Some(name), true) => {
                scope.push('.');
                scope.push_str(name);
            }
            (name, false) => scope.push_str(name.as_deref().unwrap_or("Unk")),
            (None, true) => {}
        }
        if field.kind.as_deref() == Some("array") {
            let empty = [SchemaField {
                name: None,
                kind: None,
                count: None,
                fields: None,
            }];
            let subfields = field.fields.as_deref().unwrap_or(&empty);
            for at in 0..field.count.unwrap_or(1) {
                flatten(subfields, &format!("{scope}[{at}]"), true, out);
            }
        } else {
            out.push(scope);
        }
    }
}

/// A sheet's schema field names paired with the column each one reads, which is the sheet's columns
/// in offset order rather than declaration order.
struct Named {
    columns: HashMap<String, ColumnDefinition>,
}

impl Named {
    fn new(yml: &str, sheet: &ironworks::excel::Sheet<&str>) -> Self {
        let parsed: SchemaFile = serde_yml::from_str(yml).expect("a schema");
        let mut names = Vec::new();
        flatten(&parsed.fields, "", false, &mut names);
        let mut columns = sheet.columns().expect("columns");
        columns.sort_by_key(|column| (column.offset(), column.kind() as u16));
        assert_eq!(names.len(), columns.len(), "{} moved", sheet.name());
        Self {
            columns: names.into_iter().zip(columns).collect(),
        }
    }

    fn at(&self, name: &str) -> ColumnDefinition {
        self.columns
            .get(name)
            .unwrap_or_else(|| panic!("no column {name}"))
            .clone()
    }
}

fn integer(field: &Field) -> Option<u32> {
    Some(match field {
        Field::I8(held) => u32::try_from(*held).ok()?,
        Field::I16(held) => u32::try_from(*held).ok()?,
        Field::I32(held) => u32::try_from(*held).ok()?,
        Field::I64(held) => u32::try_from(*held).ok()?,
        Field::U8(held) => u32::from(*held),
        Field::U16(held) => u32::from(*held),
        Field::U32(held) => *held,
        Field::U64(held) => u32::try_from(*held).ok()?,
        _ => return None,
    })
}

fn text(field: &Field) -> Option<String> {
    match field {
        Field::String(held) => Some(held.to_string()),
        _ => None,
    }
}

fn cutscene_instruction(name: &str) -> bool {
    ["CUTSCENE", "CUT_SCENE", "CUT_EVENT", "NCUT_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let list = args.next().expect("a path list");
    let yml = args.next().expect("a schema directory");
    let read = |name: &str| std::fs::read_to_string(format!("{yml}/{name}.yml")).expect(name);

    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    let shipped = sqpack.entries().expect("the index");
    let under_cut = shipped.iter().filter(|held| held.category == CUT).count();
    println!("index: {} files ship, {under_cut} under cut/", shipped.len());

    let listed = std::fs::read_to_string(list).expect("the path list");
    let mut by_extension: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut live = HashSet::new();
    for path in listed.lines().filter(|path| path.starts_with("cut/")) {
        let extension = path.rsplit('.').next().unwrap_or("");
        let exists = sqpack.exists(path).unwrap_or(false);
        let seen = by_extension.entry(extension).or_default();
        seen.0 += 1;
        if exists {
            seen.1 += 1;
            live.insert(path.to_owned());
        }
    }
    for (extension, (named, exists)) in &by_extension {
        println!("cut/: {named:>7} .{extension} named, {exists:>7} live");
    }
    let named_live: usize = by_extension.values().map(|(_, live)| live).sum();
    println!("cut/: {} live entries no name covers", under_cut - named_live);

    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks).with_default_language(Language::English);

    let cutscene = excel.sheet("Cutscene").expect("Cutscene");
    let path_column = Named::new(&read("Cutscene"), &cutscene).at("Path");
    let mut row_path: BTreeMap<u32, String> = BTreeMap::new();
    for row in cutscene {
        let Some(stem) = row
            .field(&path_column)
            .ok()
            .and_then(|held| text(&held))
            .filter(|stem| !stem.is_empty())
        else {
            continue;
        };
        row_path.insert(row.row_id(), format!("cut/{stem}.cutb"));
    }
    let stated: HashSet<&String> = row_path.values().collect();
    let resolving = stated.iter().filter(|path| live.contains(**path)).count();
    println!(
        "Cutscene: {} rows name a path, {} distinct, {resolving} of those ship",
        row_path.len(),
        stated.len(),
    );

    let quest = excel.sheet("Quest").expect("Quest");
    let named = Named::new(&read("Quest"), &quest);
    let params: Vec<(ColumnDefinition, ColumnDefinition)> = (0..PARAMS)
        .map(|slot| {
            (
                named.at(&format!("QuestParams[{slot}].ScriptInstruction")),
                named.at(&format!("QuestParams[{slot}].ScriptArg")),
            )
        })
        .collect();
    let mut by_quest: BTreeMap<u32, HashSet<u32>> = BTreeMap::new();
    for row in quest {
        for (instruction, arg) in &params {
            let (Some(instruction), Some(arg)) = (
                row.field(instruction).ok().and_then(|held| text(&held)),
                row.field(arg).ok().and_then(|held| integer(&held)),
            ) else {
                continue;
            };
            if cutscene_instruction(&instruction) && row_path.contains_key(&arg) {
                by_quest.entry(row.row_id()).or_default().insert(arg);
            }
        }
    }
    println!("QuestParams: {} quests name a cutscene", by_quest.len());

    let journal = excel.sheet("CompleteJournal").expect("CompleteJournal");
    let named = Named::new(&read("CompleteJournal"), &journal);
    let ordinal_column = named.at("Unknown0");
    let journal_slots: Vec<ColumnDefinition> = (0..JOURNAL_SLOTS)
        .map(|slot| named.at(&format!("Cutscene[{slot}]")))
        .collect();
    let mut by_journal: BTreeMap<u32, HashSet<u32>> = BTreeMap::new();
    for row in journal {
        let Some(ordinal) = row.field(&ordinal_column).ok().and_then(|h| integer(&h)) else {
            continue;
        };
        let held: HashSet<u32> = journal_slots
            .iter()
            .filter_map(|slot| integer(&row.field(slot).ok()?))
            .filter(|held| row_path.contains_key(held))
            .collect();
        if !held.is_empty() {
            by_journal
                .entry(FIRST_QUEST + ordinal)
                .or_default()
                .extend(held);
        }
    }
    let (mut shared, mut overlapping, mut identical) = (0, 0, 0);
    for (quest, held) in &by_journal {
        let Some(claimed) = by_quest.get(quest) else {
            continue;
        };
        shared += 1;
        overlapping += usize::from(!held.is_disjoint(claimed));
        identical += usize::from(held == claimed);
    }
    println!(
        "CompleteJournal: {} entries offer a cutscene; of the {shared} whose 65536+Unknown0 quest also names one, {overlapping} share a file and {identical} state the same set",
        by_journal.len()
    );

    let mut owners: BTreeMap<&str, HashSet<u32>> = BTreeMap::new();
    owners.insert(
        "Quest",
        by_quest
            .values()
            .chain(by_journal.values())
            .flatten()
            .copied()
            .collect(),
    );
    for (sheet, fields) in [
        ("InstanceContent", &["Cutscene"][..]),
        ("PartyContentCutscene", &["Cutscene"]),
        ("PublicContentCutscene", &["Cutscene", "Cutscene2"]),
        ("Warp", &["StartCutscene", "EndCutscene"]),
    ] {
        let opened = excel.sheet(sheet).expect(sheet);
        let named = Named::new(&read(sheet), &opened);
        let columns: Vec<ColumnDefinition> = fields.iter().map(|name| named.at(name)).collect();
        let mut rows = HashSet::new();
        for row in opened {
            for column in &columns {
                let Some(held) = row.field(column).ok().and_then(|h| integer(&h)) else {
                    continue;
                };
                if held != 0 && row_path.contains_key(&held) {
                    rows.insert(held);
                }
            }
        }
        owners.insert(sheet, rows);
    }

    let mut claimed: HashSet<&String> = HashSet::new();
    for (owner, rows) in &owners {
        let paths: HashSet<&String> = rows
            .iter()
            .filter_map(|row| row_path.get(row))
            .filter(|path| live.contains(*path))
            .collect();
        println!("{owner}: {} rows, {} live files", rows.len(), paths.len());
        claimed.extend(paths);
    }
    let journal_files = by_journal
        .values()
        .flatten()
        .filter_map(|row| row_path.get(row))
        .filter(|path| live.contains(*path))
        .collect::<HashSet<_>>()
        .len();
    println!(
        "quests: {} carry a cutscene, of which the Unending Journey offers {journal_files} files",
        by_quest
            .keys()
            .chain(by_journal.keys())
            .collect::<HashSet<_>>()
            .len(),
    );
    println!("claimed by any sheet: {} live files", claimed.len());

    let cutb: HashSet<&String> = live.iter().filter(|p| p.ends_with(".cutb")).collect();
    let remainder: Vec<&String> = cutb.difference(&claimed).copied().collect();
    let orphaned = remainder.iter().filter(|p| !stated.contains(**p)).count();
    println!(
        "unclaimed .cutb: {}, of which {orphaned} have no Cutscene row at all",
        remainder.len()
    );
    let mut by_group: BTreeMap<&str, usize> = BTreeMap::new();
    for path in &remainder {
        *by_group
            .entry(path.split('/').nth(2).unwrap_or("?"))
            .or_default() += 1;
    }
    let mut ranked: Vec<(&str, usize)> = by_group.into_iter().collect();
    ranked.sort_by_key(|(group, count)| (std::cmp::Reverse(*count), *group));
    for (group, count) in ranked.iter().take(20) {
        println!("  {count:>5}  {group}");
    }
    println!("  ({} groups unclaimed)", ranked.len());
}
