use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, anyhow, bail};
use compact_str::ToCompactString;
use ironworks::excel::Language;

use crate::{
    backend::Backend,
    excel::{
        base::BaseSheet,
        provider::{ExcelHeader, ExcelProvider, ExcelRow, ExcelSheet},
    },
    quests::{
        derive,
        graph::Graph,
        tree::{Category, Genre, Section},
    },
    schema::{Schema, provider::SchemaProvider},
    sheet::{GlobalContext, SchemaColumn, SheetColumnDefinition, TableContext, read_integer},
};

/// How many `QuestParams` slots a quest has.
const PARAMS: usize = 50;

/// How many cutscenes one `CompleteJournal` entry can offer.
const JOURNAL_CUTSCENES: usize = 24;

pub struct Quest {
    pub row_id: u32,
    pub id: String,
    /// The localized title, or the `Id` for the rows that ship without one.
    pub name: String,
    pub sort_key: u16,
    /// 2 where any one prerequisite suffices, 1 where all of them are required.
    pub join: u8,
    /// Quests this one excludes and is excluded by. Symmetric, so never a graph edge.
    pub lock: [u32; 2],
}

/// A sheet paired with the schema that names its columns.
pub struct Fields {
    pub sheet: BaseSheet,
    schema: Schema,
    columns: Vec<SheetColumnDefinition>,
    by_name: HashMap<String, u32>,
}

impl Fields {
    pub async fn load(backend: &Backend, name: &str, language: Language) -> Result<Self> {
        let sheet = backend.excel().get_sheet(name, language).await?;
        let text = backend.schema().get_schema_text(name).await?;
        let schema = Schema::from_str(&text)?
            .map_err(|errors| anyhow!("{name} schema is invalid: {}", errors.len()))?;
        let (named, _) = SchemaColumn::from_schema(&schema)?;
        let columns = SheetColumnDefinition::from_sheet(&sheet);
        if named.len() != columns.len() {
            bail!(
                "{name} schema names {} columns, the sheet has {}",
                named.len(),
                columns.len()
            );
        }
        let by_name = named
            .iter()
            .enumerate()
            .map(|(at, column)| (column.name().to_owned(), at as u32))
            .collect();
        Ok(Self {
            sheet,
            schema,
            columns,
            by_name,
        })
    }

    fn index(&self, name: &str) -> Result<u32> {
        self.by_name
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("{} has no column {name}", self.sheet.name()))
    }

    pub fn at(&self, name: &str) -> Result<&SheetColumnDefinition> {
        Ok(&self.columns[self.index(name)? as usize])
    }
}

pub fn integer(row: ExcelRow<'_>, column: &SheetColumnDefinition) -> u32 {
    read_integer::<i64>(row, u32::from(column.offset()), column.kind())
        .unwrap_or(0)
        .try_into()
        .unwrap_or(0)
}

fn text(row: ExcelRow<'_>, column: &SheetColumnDefinition) -> String {
    row.read_string(u32::from(column.offset()))
        .ok()
        .and_then(|value| value.format().try_to_compact_string().ok())
        .map_or_else(String::new, Into::into)
}

pub struct Loaded {
    quests: Vec<Quest>,
    rewatchable: HashMap<u32, Vec<u32>>,
    graph: Graph,
    sections: Vec<Section>,
    uncategorized: Vec<u32>,
    fields: Fields,
}

/// `Quest` ships no `Language::None`, so a caller that leaves the language at its default gets an
/// empty sheet rather than an error.
pub async fn load(backend: Backend, language: Language) -> Result<Loaded> {
    let fields = Fields::load(&backend, "Quest", language).await?;
    let (id, name, genre, sort_key, join) = (
        fields.at("Id")?,
        fields.at("Name")?,
        fields.at("JournalGenre")?,
        fields.at("SortKey")?,
        fields.at("PreviousQuestJoin")?,
    );
    let prev = [
        fields.at("PreviousQuest[0]")?,
        fields.at("PreviousQuest[1]")?,
        fields.at("PreviousQuest[2]")?,
    ];
    let lock = [fields.at("QuestLock[0]")?, fields.at("QuestLock[1]")?];

    let mut rows: Vec<(Quest, u32, [u32; 3])> = Vec::new();
    for row_id in fields.sheet.get_row_ids() {
        let Ok(row) = fields.sheet.get_row(row_id) else {
            continue;
        };
        let id = text(row, id);
        if id.is_empty() {
            continue;
        }
        let mut title = text(row, name);
        if title.is_empty() {
            title.clone_from(&id);
        }
        rows.push((
            Quest {
                row_id,
                id,
                name: title,
                sort_key: integer(row, sort_key) as u16,
                join: integer(row, join) as u8,
                lock: lock.map(|column| integer(row, column)),
            },
            integer(row, genre),
            prev.map(|column| integer(row, column)),
        ));
    }
    rows.sort_by_key(|(quest, ..)| quest.row_id);

    let row_ids: Vec<u32> = rows.iter().map(|(quest, ..)| quest.row_id).collect();
    let prev: Vec<[u32; 3]> = rows.iter().map(|(.., prev)| *prev).collect();
    let graph = Graph::build(&row_ids, &prev);

    let (sections, uncategorized) = journal(&backend, language, &rows).await?;
    let rewatchable = rewatchable(&backend, language).await.unwrap_or_default();
    let quests = rows.into_iter().map(|(quest, ..)| quest).collect();

    Ok(Loaded {
        quests,
        rewatchable,
        graph,
        sections,
        uncategorized,
        fields,
    })
}

/// The cutscenes the Unending Journey offers, by quest row. `CompleteJournal` numbers a quest the
/// way the quest's own id does, so the two only meet through `FIRST_ROW`.
async fn rewatchable(backend: &Backend, language: Language) -> Result<HashMap<u32, Vec<u32>>> {
    let fields = Fields::load(backend, "CompleteJournal", language).await?;
    let quest = fields.at("Unknown0")?;
    let slots = (0..JOURNAL_CUTSCENES)
        .map(|slot| fields.at(&format!("Cutscene[{slot}]")))
        .collect::<Result<Vec<_>>>()?;
    let mut found: HashMap<u32, Vec<u32>> = HashMap::new();
    for row_id in fields.sheet.get_row_ids() {
        let Ok(row) = fields.sheet.get_row(row_id) else {
            continue;
        };
        let held: Vec<u32> = slots
            .iter()
            .map(|slot| integer(row, slot))
            .filter(|held| *held != 0)
            .collect();
        if !held.is_empty() {
            found
                .entry(derive::FIRST_ROW + integer(row, quest))
                .or_default()
                .extend(held);
        }
    }
    Ok(found)
}

/// `JournalGenre -> JournalCategory -> JournalSection` is a single link at every level, so a quest
/// belongs to exactly one of each. Anything that fails to resolve falls in with the genre-0 quests
/// rather than dropping out of the tree.
async fn journal(
    backend: &Backend,
    language: Language,
    rows: &[(Quest, u32, [u32; 3])],
) -> Result<(Vec<Section>, Vec<u32>)> {
    let genres = Fields::load(backend, "JournalGenre", language).await?;
    let categories = Fields::load(backend, "JournalCategory", language).await?;
    let sections = Fields::load(backend, "JournalSection", language).await?;
    let (genre_name, genre_category) = (genres.at("Name")?, genres.at("JournalCategory")?);
    let (category_name, category_section) =
        (categories.at("Name")?, categories.at("JournalSection")?);
    let section_name = sections.at("Name")?;

    let mut chain: HashMap<u32, (u32, u32)> = HashMap::new();
    let mut genre_names: HashMap<u32, String> = HashMap::new();
    let mut category_names: HashMap<u32, String> = HashMap::new();
    let mut section_names: HashMap<u32, String> = HashMap::new();
    for genre in rows.iter().map(|(_, genre, _)| *genre).filter(|g| *g != 0) {
        if chain.contains_key(&genre) {
            continue;
        }
        let Ok(genre_row) = genres.sheet.get_row(genre) else {
            continue;
        };
        let category = integer(genre_row, genre_category);
        let Ok(category_row) = categories.sheet.get_row(category) else {
            continue;
        };
        let section = integer(category_row, category_section);
        let Ok(section_row) = sections.sheet.get_row(section) else {
            continue;
        };
        chain.insert(genre, (category, section));
        genre_names.insert(genre, text(genre_row, genre_name));
        category_names.insert(category, text(category_row, category_name));
        section_names.insert(section, text(section_row, section_name));
    }

    // No level of the tree has a sort column, so each is ordered by row id and the quests under a
    // genre by `SortKey`, whose ties are real.
    let mut order: Vec<u32> = (0..rows.len() as u32).collect();
    order.sort_by_key(|node| {
        let (quest, ..) = &rows[*node as usize];
        (quest.sort_key, quest.row_id)
    });

    type Tree = BTreeMap<u32, BTreeMap<u32, BTreeMap<u32, Vec<u32>>>>;
    let mut tree = Tree::new();
    let mut uncategorized = Vec::new();
    for node in order {
        let genre = rows[node as usize].1;
        match chain.get(&genre) {
            Some(&(category, section)) => tree
                .entry(section)
                .or_default()
                .entry(category)
                .or_default()
                .entry(genre)
                .or_default()
                .push(node),
            None => uncategorized.push(node),
        }
    }

    let sections = tree
        .into_iter()
        .map(|(section, categories)| Section {
            name: section_names[&section].clone(),
            categories: categories
                .into_iter()
                .map(|(category, genres)| Category {
                    name: category_names[&category].clone(),
                    genres: genres
                        .into_iter()
                        .map(|(genre, quests)| Genre {
                            name: genre_names[&genre].clone(),
                            quests,
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    Ok((sections, uncategorized))
}

pub struct Index {
    pub quests: Vec<Quest>,
    pub rewatchable: HashMap<u32, Vec<u32>>,
    pub graph: Graph,
    pub sections: Vec<Section>,
    pub uncategorized: Vec<u32>,
    pub table: TableContext,
    fields: Fields,
}

impl Index {
    pub fn new(global: GlobalContext, loaded: Loaded) -> Self {
        let table = TableContext::new(
            global,
            loaded.fields.sheet.clone(),
            Some(&loaded.fields.schema),
        );
        Self {
            quests: loaded.quests,
            rewatchable: loaded.rewatchable,
            graph: loaded.graph,
            sections: loaded.sections,
            uncategorized: loaded.uncategorized,
            table,
            fields: loaded.fields,
        }
    }

    pub fn node_of(&self, row_id: u32) -> Option<u32> {
        self.quests
            .binary_search_by_key(&row_id, |quest| quest.row_id)
            .ok()
            .map(|at| at as u32)
    }

    pub fn quest(&self, node: u32) -> &Quest {
        &self.quests[node as usize]
    }

    pub fn row(&self, node: u32) -> Option<ExcelRow<'_>> {
        self.fields.sheet.get_row(self.quest(node).row_id).ok()
    }

    /// The offset index of a column, for reading it through the shared cell machinery.
    pub fn column(&self, name: &str) -> Option<u32> {
        self.fields.by_name.get(name).copied()
    }

    /// The `Cutscene` and `BGM` rows a quest's script parameters name, each with the instruction
    /// naming it, which is what the script reads them by.
    ///
    /// The rewatchable entries come from `CompleteJournal` rather than an instruction, so they
    /// carry an empty name.
    pub fn assets(&self, node: u32) -> Vec<(String, derive::Param, u32)> {
        let Some(row) = self.row(node) else {
            return Vec::new();
        };
        (0..PARAMS)
            .filter_map(|slot| {
                let instruction = text(
                    row,
                    self.fields
                        .at(&format!("QuestParams[{slot}].ScriptInstruction"))
                        .ok()?,
                );
                let param = derive::param_of(&instruction)?;
                let arg = integer(
                    row,
                    self.fields
                        .at(&format!("QuestParams[{slot}].ScriptArg"))
                        .ok()?,
                );
                Some((instruction, param, arg))
            })
            .chain(
                self.rewatchable
                    .get(&self.quest(node).row_id)
                    .into_iter()
                    .flatten()
                    .map(|row| (String::new(), derive::Param::Cutscene, *row)),
            )
            .collect()
    }

    /// Every `Cutscene` row any quest names, paired with the quest. The columns are resolved once
    /// because this reads the whole sheet, unlike `assets`.
    pub fn cutscenes(&self) -> Vec<(u32, u32)> {
        let Ok(slots) = (0..PARAMS)
            .map(|slot| {
                Ok((
                    self.fields
                        .at(&format!("QuestParams[{slot}].ScriptInstruction"))?
                        .clone(),
                    self.fields
                        .at(&format!("QuestParams[{slot}].ScriptArg"))?
                        .clone(),
                ))
            })
            .collect::<Result<Vec<_>>>()
        else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for quest in &self.quests {
            if let Ok(row) = self.fields.sheet.get_row(quest.row_id) {
                for (instruction, arg) in &slots {
                    let named = row
                        .read_string(u32::from(instruction.offset()))
                        .ok()
                        .and_then(|held| str::from_utf8(held.as_bytes()).ok())
                        .is_some_and(|held| {
                            derive::param_of(held) == Some(derive::Param::Cutscene)
                        });
                    if named {
                        found.push((quest.row_id, integer(row, arg)));
                    }
                }
            }
            found.extend(
                self.rewatchable
                    .get(&quest.row_id)
                    .into_iter()
                    .flatten()
                    .map(|row| (quest.row_id, *row)),
            );
        }
        found
    }
}
