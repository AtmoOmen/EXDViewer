use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use ironworks::excel::Language;

use crate::{
    backend::Backend,
    excel::provider::ExcelSheet,
    quests::{
        derive,
        index::{Fields, integer},
    },
};

/// What claims a cutscene. A file can be claimed more than once, and an instance cutscene a quest
/// also names is the case worth seeing rather than one worth hiding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    Quest(u32),
    Instance(u32),
    PartyContent(u32),
    PublicContent(u32),
    Warp(u32),
}

impl Owner {
    pub fn sheet(self) -> &'static str {
        match self {
            Self::Quest(_) => "Quest",
            Self::Instance(_) => "InstanceContent",
            Self::PartyContent(_) => "PartyContentCutscene",
            Self::PublicContent(_) => "PublicContentCutscene",
            Self::Warp(_) => "Warp",
        }
    }

    pub fn row(self) -> u32 {
        match self {
            Self::Quest(row)
            | Self::Instance(row)
            | Self::PartyContent(row)
            | Self::PublicContent(row)
            | Self::Warp(row) => row,
        }
    }
}

pub struct Entry {
    pub path: String,
    /// The `Cutscene` row that names the file, where one does.
    pub row: Option<u32>,
    pub owners: Vec<Owner>,
}

pub struct Cutscenes {
    pub entries: Vec<Entry>,
    pub owned: usize,
}

/// Every `.cutb` the install holds, with whatever claims it. `shipping` is the file list rather
/// than the sheet's, so a cutscene no row names still gets a shelf.
pub async fn load(
    backend: Backend,
    language: Language,
    shipping: Vec<String>,
    quests: Vec<(u32, u32)>,
) -> Result<Cutscenes> {
    let cutscene = Fields::load(&backend, "Cutscene", language).await?;
    let path = cutscene.at("Path")?;
    let mut named: BTreeMap<String, u32> = BTreeMap::new();
    for row_id in cutscene.sheet.get_row_ids() {
        let Ok(row) = cutscene.sheet.get_row(row_id) else {
            continue;
        };
        let Ok(stem) = row.read_string(u32::from(path.offset())) else {
            continue;
        };
        let Ok(stem) = str::from_utf8(stem.as_bytes()) else {
            continue;
        };
        if !stem.is_empty() {
            named.insert(derive::cutscene_path(stem), row_id);
        }
    }

    let mut owners: BTreeMap<u32, BTreeSet<Owner>> = BTreeMap::new();
    for (quest, row) in quests {
        owners.entry(row).or_default().insert(Owner::Quest(quest));
    }
    for (sheet, columns, owner) in [
        (
            "InstanceContent",
            &["Cutscene"][..],
            Owner::Instance as fn(u32) -> Owner,
        ),
        ("PartyContentCutscene", &["Cutscene"], Owner::PartyContent),
        (
            "PublicContentCutscene",
            &["Cutscene", "Cutscene2"],
            Owner::PublicContent,
        ),
        ("Warp", &["StartCutscene", "EndCutscene"], Owner::Warp),
    ] {
        let Ok(fields) = Fields::load(&backend, sheet, language).await else {
            continue;
        };
        let Ok(columns) = columns
            .iter()
            .map(|name| fields.at(name).cloned())
            .collect::<Result<Vec<_>>>()
        else {
            continue;
        };
        for row_id in fields.sheet.get_row_ids() {
            let Ok(row) = fields.sheet.get_row(row_id) else {
                continue;
            };
            for column in &columns {
                let held = integer(row, column);
                if held != 0 {
                    owners.entry(held).or_default().insert(owner(row_id));
                }
            }
        }
    }

    let mut entries: Vec<Entry> = shipping
        .into_iter()
        .map(|path| {
            let row = named.get(&path).copied();
            let owners = row
                .and_then(|row| owners.get(&row))
                .map(|held| held.iter().copied().collect())
                .unwrap_or_default();
            Entry { path, row, owners }
        })
        .collect();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let owned = entries
        .iter()
        .filter(|entry| !entry.owners.is_empty())
        .count();
    Ok(Cutscenes { entries, owned })
}
