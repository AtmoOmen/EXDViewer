//! What a quest script names, and how much of it the install can resolve.
//!
//! Every `self.X` a scene handler reads is a row key in the quest's own text sheet, a
//! `QuestParams` instruction, or neither.
//!
//! `quest_scripts <EXDSchema directory> [quest limit]`

use std::collections::{BTreeMap, HashMap, HashSet};

use ironworks::{
    Ironworks,
    excel::{Excel, Field, Language},
    file::exh::ColumnDefinition,
    sqpack::{Install, SqPack},
};
use luadec::{Chunk, Expr, Stat};
use serde::Deserialize;
use viewer::quests::script::{self, Step};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PARAMS: usize = 50;
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

fn named(yml: &str, sheet: &ironworks::excel::Sheet<&str>) -> HashMap<String, ColumnDefinition> {
    let parsed: SchemaFile = serde_yml::from_str(yml).expect("a schema");
    let mut names = Vec::new();
    flatten(&parsed.fields, "", false, &mut names);
    let mut columns = sheet.columns().expect("columns");
    columns.sort_by_key(|column| (column.offset(), column.kind() as u16));
    assert_eq!(names.len(), columns.len(), "{} moved", sheet.name());
    names.into_iter().zip(columns).collect()
}

fn integer(field: &Field) -> Option<u32> {
    Some(match field {
        Field::I8(held) => u32::try_from(*held).ok()?,
        Field::I16(held) => u32::try_from(*held).ok()?,
        Field::I32(held) => u32::try_from(*held).ok()?,
        Field::U8(held) => u32::from(*held),
        Field::U16(held) => u32::from(*held),
        Field::U32(held) => *held,
        _ => return None,
    })
}

fn text(field: &Field) -> Option<String> {
    match field {
        Field::String(held) => Some(held.to_string()),
        _ => None,
    }
}

/// Every `t.key` and `t:method(...)` the block reads, gathered depth first.
#[derive(Default)]
struct Reads {
    keys: Vec<String>,
    methods: Vec<String>,
    /// A method call and the `self.X` names its arguments read, in order.
    calls: Vec<(String, Vec<String>)>,
    raw: usize,
}

/// The `t.KEY` names an argument list reads, in order.
fn arguments(held: &[Expr]) -> Vec<String> {
    held.iter()
        .filter_map(|argument| match argument {
            Expr::Index(_, key) => match key.as_ref() {
                Expr::Str(name) => Some(String::from_utf8_lossy(name).into_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

impl Reads {
    fn block(&mut self, block: &[Stat]) {
        for stat in block {
            match stat {
                Stat::Local(_, values) | Stat::Return(values) => self.exprs(values),
                Stat::Assign(targets, values) => {
                    for target in targets {
                        if let luadec::Target::Index(table, key) = target {
                            self.expr(table);
                            self.expr(key);
                        }
                    }
                    self.exprs(values);
                }
                Stat::Call(call) => self.expr(call),
                Stat::Do(body) => self.block(body),
                Stat::If(arms, otherwise) => {
                    for (condition, body) in arms {
                        self.expr(condition);
                        self.block(body);
                    }
                    if let Some(body) = otherwise {
                        self.block(body);
                    }
                }
                Stat::While(condition, body) => {
                    self.expr(condition);
                    self.block(body);
                }
                Stat::Repeat(body, condition) => {
                    self.block(body);
                    self.expr(condition);
                }
                Stat::NumericFor {
                    start,
                    limit,
                    step,
                    body,
                    ..
                } => {
                    self.exprs(std::slice::from_ref(start));
                    self.exprs(std::slice::from_ref(limit));
                    self.exprs(std::slice::from_ref(step));
                    self.block(body);
                }
                Stat::GenericFor { values, body, .. } => {
                    self.exprs(values);
                    self.block(body);
                }
                Stat::Raw(_) => self.raw += 1,
                Stat::Break => {}
            }
        }
    }

    fn exprs(&mut self, held: &[Expr]) {
        for expr in held {
            self.expr(expr);
        }
    }

    fn expr(&mut self, held: &Expr) {
        match held {
            Expr::Index(table, key) => {
                self.expr(table);
                match key.as_ref() {
                    Expr::Str(name) => self.keys.push(String::from_utf8_lossy(name).into_owned()),
                    other => self.expr(other),
                }
            }
            Expr::Call(callee, arguments) => {
                self.expr(callee);
                self.exprs(arguments);
            }
            Expr::Method(object, name, held) => {
                self.expr(object);
                self.methods.push(name.clone());
                self.calls.push((name.clone(), arguments(held)));
                self.exprs(held);
            }
            Expr::Binary(_, left, right) => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Unary(_, held) => self.expr(held),
            Expr::Table { array, hash } => {
                self.exprs(array);
                for (key, value) in hash {
                    self.expr(key);
                    self.expr(value);
                }
            }
            Expr::Function(closure) => self.block(&closure.body),
            _ => {}
        }
    }
}

/// The `OnSceneNNNNN` handlers a unit assigns, by the number in the name.
fn scenes(block: &[Stat], out: &mut BTreeMap<u32, Vec<Stat>>) {
    for stat in block {
        let Stat::Assign(targets, values) = stat else {
            continue;
        };
        let (Some(luadec::Target::Index(_, key)), Some(Expr::Function(closure))) =
            (targets.first(), values.first())
        else {
            continue;
        };
        let Expr::Str(name) = key else { continue };
        let name = String::from_utf8_lossy(name);
        let Some(number) = name
            .strip_prefix("OnScene")
            .and_then(|held| held.parse::<u32>().ok())
        else {
            continue;
        };
        out.insert(number, closure.body.clone());
    }
}

/// Count what the shipped reader made of a scene, arms included.
fn tally(
    steps: &[Step],
    keys: &HashSet<String>,
    kinds: &mut BTreeMap<&'static str, usize>,
    lines: &mut usize,
    resolved: &mut usize,
) {
    for step in steps {
        let name = match step {
            Step::Line { keys: held, .. } => {
                *lines += 1;
                *resolved += usize::from(held.iter().any(|key| keys.contains(key)));
                "line"
            }
            Step::Wait(_) => "wait",
            Step::Cutscene(_) => "cutscene",
            Step::Bgm(_) => "bgm",
            Step::Fade { .. } => "fade",
            Step::Branch { arms, .. } => {
                for arm in arms {
                    tally(&arm.steps, keys, kinds, lines, resolved);
                }
                "branch"
            }
            Step::Other(_) => "other",
        };
        *kinds.entry(name).or_default() += 1;
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let yml = args.next().expect("a schema directory");
    let limit: usize = args
        .next()
        .and_then(|held| held.parse().ok())
        .unwrap_or(usize::MAX);

    let sqpack = SqPack::new(Install::at_sqpack(SQPACK));
    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks.clone()).with_default_language(Language::English);
    let quest = excel.sheet("Quest").expect("Quest");
    let columns = named(
        &std::fs::read_to_string(format!("{yml}/Quest.yml")).expect("Quest.yml"),
        &quest,
    );
    let id_column = columns["Id"].clone();
    let instructions: Vec<ColumnDefinition> = (0..PARAMS)
        .map(|slot| columns[&format!("QuestParams[{slot}].ScriptInstruction")].clone())
        .collect();
    let arguments: Vec<ColumnDefinition> = (0..PARAMS)
        .map(|slot| columns[&format!("QuestParams[{slot}].ScriptArg")].clone())
        .collect();

    let (mut quests, mut present, mut parsed, mut whole) = (0, 0, 0, 0);
    let (mut handlers, mut with_talk, mut empty) = (0, 0, 0);
    let (mut as_text, mut as_param, mut unresolved) = (0, 0, 0);
    let (mut talks, mut talks_resolved) = (0, 0);
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut modelled_scenes, mut model_lines, mut model_lines_resolved) = (0, 0, 0);
    let (mut cuts, mut cuts_param, mut cuts_live) = (0, 0, 0);
    let mut quests_with_cut = 0;
    let mut missed: BTreeMap<String, usize> = BTreeMap::new();
    let mut called: BTreeMap<String, usize> = BTreeMap::new();
    let mut text_sheets = 0;
    let mut params_hit: BTreeMap<String, usize> = BTreeMap::new();

    let cutscene = excel.sheet("Cutscene").expect("Cutscene");
    let cutscene_path = named(
        &std::fs::read_to_string(format!("{yml}/Cutscene.yml")).expect("Cutscene.yml"),
        &cutscene,
    )["Path"]
        .clone();
    let mut cut_rows: HashMap<u32, String> = HashMap::new();
    for line in cutscene {
        if let Some(stem) = line
            .field(&cutscene_path)
            .ok()
            .and_then(|held| text(&held))
            .filter(|stem| !stem.is_empty())
        {
            cut_rows.insert(line.row_id(), format!("cut/{stem}.cutb"));
        }
    }

    for row in quest {
        let Some(id) = row
            .field(&id_column)
            .ok()
            .and_then(|held| text(&held))
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        quests += 1;
        if quests > limit {
            break;
        }
        let folder = row.row_id().saturating_sub(FIRST_QUEST) / 100;
        let Ok(bytes) = ironworks.file::<Vec<u8>>(&format!("game_script/quest/{folder:03}/{id}.luab"))
        else {
            continue;
        };
        present += 1;
        let Ok(chunk) = Chunk::parse(&bytes) else {
            continue;
        };
        parsed += 1;

        let units: Vec<luadec::Closure> = match luadec::units(&chunk) {
            Some(units) => units.iter().map(luadec::read).collect(),
            None => vec![luadec::read(chunk.main())],
        };
        let mut found = BTreeMap::new();
        for unit in &units {
            scenes(&unit.body, &mut found);
        }

        let params: HashMap<String, u32> = instructions
            .iter()
            .zip(&arguments)
            .filter_map(|(instruction, argument)| {
                let name = text(&row.field(instruction).ok()?)?;
                let held = integer(&row.field(argument).ok()?)?;
                (!name.is_empty()).then_some((name, held))
            })
            .collect();

        // The quest's own text sheet ships no schema and reads as two raw string columns.
        let mut keys: HashSet<String> = HashSet::new();
        if let Ok(sheet) = excel.sheet(format!("quest/{folder:03}/{id}")) {
            if let Ok(mut sheet_columns) = sheet.columns() {
                sheet_columns.sort_by_key(|column| (column.offset(), column.kind() as u16));
                if let Some(key) = sheet_columns.first() {
                    text_sheets += 1;
                    for line in sheet {
                        if let Some(held) = line.field(key).ok().and_then(|held| text(&held)) {
                            keys.insert(held);
                        }
                    }
                }
            }
        }

        if let Ok(read) = script::read(&bytes) {
            modelled_scenes += read.scenes.len();
            for scene in &read.scenes {
                tally(&scene.steps, &keys, &mut kinds, &mut model_lines, &mut model_lines_resolved);
            }
        }

        let mut file_raw = 0;
        let mut found_cut = false;
        for body in found.values() {
            handlers += 1;
            let mut reads = Reads::default();
            reads.block(body);
            file_raw += reads.raw;
            if body.is_empty() {
                empty += 1;
            }
            if reads
                .methods
                .iter()
                .any(|name| name == "Talk" || name == "SystemTalk")
            {
                with_talk += 1;
            }
            for name in &reads.methods {
                *called.entry(name.clone()).or_default() += 1;
            }
            for (name, held) in &reads.calls {
                match name.as_str() {
                    "Talk" | "SystemTalk" => {
                        talks += 1;
                        talks_resolved +=
                            usize::from(held.iter().any(|argument| keys.contains(argument)));
                    }
                    "PlayCutScene" => {
                        cuts += 1;
                        let row = held.iter().find_map(|argument| params.get(argument));
                        cuts_param += usize::from(row.is_some());
                        cuts_live += usize::from(
                            row.and_then(|row| cut_rows.get(row))
                                .is_some_and(|path| sqpack.exists(path).unwrap_or(false)),
                        );
                        found_cut |= row.is_some();
                    }
                    _ => {}
                }
            }
            for key in &reads.keys {
                if keys.contains(key) {
                    as_text += 1;
                } else if params.contains_key(key) {
                    as_param += 1;
                    *params_hit.entry(key.clone()).or_default() += 1;
                } else {
                    unresolved += 1;
                    *missed.entry(key.clone()).or_default() += 1;
                }
            }
        }
        if file_raw == 0 {
            whole += 1;
        }
        quests_with_cut += usize::from(found_cut);
    }

    println!("quests {quests}, scripts present {present}, parsed {parsed}, whole readings {whole}");
    println!("text sheets read {text_sheets}");
    println!("scene handlers {handlers}, with a talk {with_talk}, empty {empty}");
    println!("references: {as_text} text rows, {as_param} params, {unresolved} neither");
    println!("talk calls {talks}, resolving to a row {talks_resolved}");
    println!(
        "PlayCutScene {cuts}, naming a param {cuts_param}, whose file ships {cuts_live}; \
         quests holding one {quests_with_cut}"
    );
    println!(
        "shipped reader: {modelled_scenes} scenes, {model_lines} lines, {model_lines_resolved} \
         of them resolving"
    );
    println!("step kinds:");
    let mut ranked: Vec<(&&str, &usize)> = kinds.iter().collect();
    ranked.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), **name));
    for (name, count) in &ranked {
        println!("  {count:>8} {name}");
    }
    println!("top params read:");
    let mut ranked: Vec<(&String, &usize)> = params_hit.iter().collect();
    ranked.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), (*name).clone()));
    for (name, count) in ranked.iter().take(20) {
        println!("  {count:>7} {name}");
    }
    println!("top unresolved:");
    let mut ranked: Vec<(&String, &usize)> = missed.iter().collect();
    ranked.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), (*name).clone()));
    for (name, count) in ranked.iter().take(30) {
        println!("  {count:>7} {name}");
    }
    println!("top methods:");
    let mut ranked: Vec<(&String, &usize)> = called.iter().collect();
    ranked.sort_by_key(|(name, count)| (std::cmp::Reverse(**count), (*name).clone()));
    for (name, count) in ranked.iter().take(40) {
        println!("  {count:>7} {name}");
    }
}
