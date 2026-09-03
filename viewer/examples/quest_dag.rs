//! How well the prerequisite graph lays out: how wide it gets, how far an edge travels and how
//! often two edges between the same pair of ranks cross.
//!
//! `quest_dag <EXDSchema directory>`

use std::collections::{BTreeMap, HashMap};

use ironworks::{
    Ironworks,
    excel::{Excel, Field, Language},
    file::exh::ColumnDefinition,
    sqpack::{Install, SqPack},
};
use serde::Deserialize;
use viewer::quests::graph::Graph;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

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

fn integer(field: &Field) -> u32 {
    match field {
        Field::I8(held) => u32::try_from(*held).unwrap_or(0),
        Field::I16(held) => u32::try_from(*held).unwrap_or(0),
        Field::I32(held) => u32::try_from(*held).unwrap_or(0),
        Field::U8(held) => u32::from(*held),
        Field::U16(held) => u32::from(*held),
        Field::U32(held) => *held,
        _ => 0,
    }
}

fn main() {
    let yml = std::env::args().nth(1).expect("a schema directory");
    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks).with_default_language(Language::English);
    let quest = excel.sheet("Quest").expect("Quest");

    let parsed: SchemaFile =
        serde_yml::from_str(&std::fs::read_to_string(format!("{yml}/Quest.yml")).expect("Quest.yml"))
            .expect("a schema");
    let mut names = Vec::new();
    flatten(&parsed.fields, "", false, &mut names);
    let mut columns = quest.columns().expect("columns");
    columns.sort_by_key(|column| (column.offset(), column.kind() as u16));
    let columns: HashMap<String, ColumnDefinition> = names.into_iter().zip(columns).collect();
    let id = columns["Id"].clone();
    let previous: Vec<ColumnDefinition> = (0..3)
        .map(|slot| columns[&format!("PreviousQuest[{slot}]")].clone())
        .collect();

    let mut rows: Vec<(u32, [u32; 3])> = Vec::new();
    for row in quest {
        let named = matches!(row.field(&id), Ok(Field::String(held)) if !held.to_string().is_empty());
        if !named {
            continue;
        }
        let mut prev = [0; 3];
        for (slot, column) in previous.iter().enumerate() {
            prev[slot] = row.field(column).map(|held| integer(&held)).unwrap_or(0);
        }
        rows.push((row.row_id(), prev));
    }
    rows.sort_by_key(|(row_id, _)| *row_id);

    let row_ids: Vec<u32> = rows.iter().map(|(row_id, _)| *row_id).collect();
    let prev: Vec<[u32; 3]> = rows.iter().map(|(_, prev)| *prev).collect();
    let graph = Graph::build(&row_ids, &prev);

    println!(
        "{} quests, {} edges, {} components, {} dangling, {} cyclic",
        graph.len(),
        graph.edge_count(),
        graph.component_count(),
        graph.dangling(),
        graph.cyclic(),
    );
    let (ranks, width) = graph.extent(0);
    println!(
        "giant component: {} quests, {} ranks, {} wide",
        graph.component_nodes(0).len(),
        ranks + 1,
        width + 1,
    );

    let mut edges: Vec<(u32, u32)> = Vec::new();
    for node in 0..graph.len() as u32 {
        for prereq in graph.prereqs(node) {
            edges.push((*prereq, node));
        }
    }
    let span: u32 = edges
        .iter()
        .map(|(from, to)| graph.slot(*from).abs_diff(graph.slot(*to)))
        .sum();
    println!(
        "edges travel {span} slots in all, {:.2} each",
        f64::from(span) / edges.len() as f64
    );

    // Crossings between edges joining the same pair of ranks, which is what ordering a rank by the
    // barycenter of its prerequisites is for.
    let mut layers: BTreeMap<(u32, u32), Vec<(u32, u32)>> = BTreeMap::new();
    for (from, to) in &edges {
        layers
            .entry((graph.rank(*from), graph.rank(*to)))
            .or_default()
            .push((graph.slot(*from), graph.slot(*to)));
    }
    let crossings: usize = layers
        .values()
        .map(|held| {
            let mut count = 0;
            for (at, left) in held.iter().enumerate() {
                for right in &held[at + 1..] {
                    let flipped = (left.0 as i64 - right.0 as i64) * (left.1 as i64 - right.1 as i64);
                    count += usize::from(flipped < 0);
                }
            }
            count
        })
        .sum();
    println!("{crossings} crossings over {} rank pairs", layers.len());
}
