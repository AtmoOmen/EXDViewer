use anyhow::{Result, anyhow};
use compact_str::ToCompactString;
use egui::{CollapsingHeader, Color32, Label, RichText, ScrollArea, Sense};
use ironworks::{excel::Language, sestring::SeStr};

use crate::{
    backend::Backend,
    excel::{
        base::CachedProvider,
        provider::{ExcelProvider, ExcelRow, ExcelSheet},
    },
    quests::{
        Load,
        derive::{self, Line, Param},
        index::Index,
    },
    settings::EVALUATE_STRINGS,
    sheet::{CellResponse, SheetColumnDefinition},
};

const IDENTITY: &[&str] = &[
    "Id",
    "Expansion",
    "PlaceName",
    "JournalGenre",
    "SortKey",
    "Icon",
    "IconSpecial",
    "EventIconType",
];

const REQUIREMENTS: &[&str] = &[
    "ClassJobCategory0",
    "ClassJobLevel[0]",
    "ClassJobCategory1",
    "ClassJobLevel[1]",
    "LevelMax",
    "QuestLevelOffset",
    "ClassJobRequired",
    "ClassJobUnlock",
    "GrandCompany",
    "GrandCompanyRank",
    "BeastTribe",
    "BeastReputationRank",
    "BeastReputationValue",
    "MountRequired",
    "Festival",
    "IsHouseRequired",
];

const FLOW: &[&str] = &[
    "IssuerStart",
    "IssuerLocation",
    "TargetEnd",
    "InstanceContent[0]",
    "InstanceContent[1]",
    "InstanceContent[2]",
    "InstanceContentUnlock",
    "DeliveryQuest",
    "SatisfactionNpc",
    "SatisfactionLevel",
    "IsRepeatable",
    "RepeatIntervalType",
    "QuestRepeatFlag",
    "DailyQuestPool",
    "CanCancel",
    "Introduction",
    "HideOfferIcon",
    "HideInScenarioGuide",
];

/// `Reward[i]` resolves through `ItemRewardType`, so it is a link to an item, a class-job reward or
/// a beast-tribe bonus depending on the row.
fn reward_fields() -> Vec<String> {
    let mut names: Vec<String> = [
        "GilReward",
        "ExpFactor",
        "CurrencyReward",
        "CurrencyRewardCount",
    ]
    .iter()
    .map(|name| (*name).to_string())
    .collect();
    for i in 0..7 {
        names.push(format!("Reward[{i}]"));
        names.push(format!("ItemCountReward[{i}]"));
        names.push(format!("RewardStain[{i}]"));
    }
    for i in 0..5 {
        names.push(format!("OptionalItemReward[{i}]"));
        names.push(format!("OptionalItemCountReward[{i}]"));
        names.push(format!("OptionalItemStainReward[{i}]"));
        names.push(format!("OptionalItemIsHQReward[{i}]"));
    }
    for i in 0..3 {
        names.push(format!("ItemCatalyst[{i}]"));
        names.push(format!("ItemCountCatalyst[{i}]"));
    }
    names.extend(
        [
            "EmoteReward",
            "ActionReward",
            "GeneralActionReward[0]",
            "GeneralActionReward[1]",
            "SystemReward[0]",
            "SystemReward[1]",
            "GCTypeReward",
            "OtherReward",
            "QuestRewardOtherDisplay",
            "Tomestone",
            "TomestoneReward",
            "TomestoneCountReward",
            "ReputationReward",
        ]
        .iter()
        .map(|name| (*name).to_string()),
    );
    names
}

pub struct Dialogue {
    /// The three fixed buckets first, then one group per speaker in the order they first talk.
    groups: Vec<(String, Vec<Vec<u8>>)>,
    lines: usize,
}

pub struct Links {
    script: String,
    music: Vec<String>,
    cutscenes: Vec<String>,
}

#[derive(Default)]
pub struct Detail {
    node: Option<u32>,
    dialogue: Load<Dialogue>,
    links: Load<Links>,
}

pub enum Action {
    Select(u32),
    Navigate(String),
}

impl Detail {
    /// Dialogue and the asset links are per quest and neither is cheap, so both wait for a
    /// selection rather than being built with the index.
    pub fn poll(&mut self, backend: &Backend, index: &Index, node: u32, language: Language) {
        if self.node != Some(node) {
            self.node = Some(node);
            self.dialogue = Load::Idle;
            self.links = Load::Idle;
        }
        let quest = index.quest(node);
        if matches!(self.dialogue, Load::Idle) {
            let excel = backend.excel().clone();
            let name = derive::text_sheet(quest.row_id, &quest.id);
            let id = quest.id.to_uppercase();
            self.dialogue = Load::spawn(async move { dialogue(excel, name, id, language).await });
        }
        if matches!(self.links, Load::Idle) {
            let backend = backend.clone();
            let script = derive::script_path(quest.row_id, &quest.id);
            let params = index.assets(node);
            self.links = Load::spawn(async move { links(backend, language, script, params).await });
        }
        self.dialogue.poll();
        self.links.poll();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, index: &Index, node: u32) -> Option<Action> {
        let quest = index.quest(node);
        let mut action = None;

        ui.label(RichText::new(&quest.name).heading());
        ui.label(
            RichText::new(format!("{} · row {}", quest.id, quest.row_id))
                .weak()
                .small(),
        );
        ui.add_space(6.0);

        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            action = self.body(ui, index, node);
        });
        action
    }

    fn body(&mut self, ui: &mut egui::Ui, index: &Index, node: u32) -> Option<Action> {
        let Some(row) = index.row(node) else {
            ui.colored_label(Color32::RED, "The quest's row went away");
            return None;
        };
        let mut action = fields(
            ui,
            index,
            row,
            IDENTITY.iter().map(|name| (*name).to_string()),
        );

        action = action.or(section(ui, "Requirements", false, |ui| {
            fields(
                ui,
                index,
                row,
                REQUIREMENTS.iter().map(|n| (*n).to_string()),
            )
        }));
        action = action.or(section(ui, "Progression", false, |ui| {
            fields(ui, index, row, FLOW.iter().map(|n| (*n).to_string()))
        }));
        action = action.or(section(ui, "Rewards", true, |ui| {
            fields(ui, index, row, reward_fields().into_iter())
        }));

        action = action.or(self.relations(ui, index, node));
        action = action.or(self.files(ui));
        action = action.or(self.dialogue(ui));
        action
    }

    fn relations(&self, ui: &mut egui::Ui, index: &Index, node: u32) -> Option<Action> {
        let mut action = None;
        let prereqs = index.graph.prereqs(node);
        if !prereqs.is_empty() {
            let any = index.quest(node).join == 2;
            let title = if any && prereqs.len() > 1 {
                "Requires any one of"
            } else {
                "Requires"
            };
            action = action.or(section(ui, title, true, |ui| {
                quest_list(ui, index, prereqs)
            }));
        }
        let dependents = index.graph.dependents(node);
        if !dependents.is_empty() {
            action = action.or(section(ui, "Unlocks", true, |ui| {
                quest_list(ui, index, dependents)
            }));
        }
        let locks: Vec<u32> = index
            .quest(node)
            .lock
            .iter()
            .filter_map(|row_id| index.node_of(*row_id))
            .collect();
        if !locks.is_empty() {
            action = action.or(section(ui, "Alternatives", true, |ui| {
                ui.label(
                    RichText::new("Taking one of these puts the others out of reach.")
                        .weak()
                        .small(),
                );
                quest_list(ui, index, &locks)
            }));
        }
        action
    }

    fn files(&self, ui: &mut egui::Ui) -> Option<Action> {
        let title = match &self.links {
            Load::Ready(links) => {
                format!("Files ({})", 1 + links.music.len() + links.cutscenes.len())
            }
            _ => "Files".to_string(),
        };
        section(ui, &title, true, |ui| match &self.links {
            Load::Idle | Load::Loading(_) => {
                ui.spinner();
                None
            }
            Load::Failed(error) => {
                ui.colored_label(Color32::RED, error.clone());
                None
            }
            Load::Ready(links) => {
                let mut action = asset_link(ui, &links.script);
                for path in links.music.iter().chain(&links.cutscenes) {
                    action = action.or(asset_link(ui, path));
                }
                action
            }
        })
    }

    fn dialogue(&self, ui: &mut egui::Ui) -> Option<Action> {
        let title = match &self.dialogue {
            Load::Ready(dialogue) => format!("Dialogue ({})", dialogue.lines),
            _ => "Dialogue".to_string(),
        };
        section(ui, &title, false, |ui| {
            match &self.dialogue {
                Load::Idle | Load::Loading(_) => {
                    ui.spinner();
                }
                Load::Failed(error) => {
                    ui.colored_label(Color32::RED, error.clone());
                }
                Load::Ready(dialogue) => {
                    for (speaker, lines) in &dialogue.groups {
                        ui.add_space(4.0);
                        ui.label(RichText::new(speaker).strong());
                        for line in lines {
                            ui.label(sestring(ui, line));
                        }
                    }
                }
            }
            None
        })
    }
}

/// A collapsing section that only exists when its body drew something.
fn section(
    ui: &mut egui::Ui,
    title: &str,
    open: bool,
    body: impl FnOnce(&mut egui::Ui) -> Option<Action>,
) -> Option<Action> {
    CollapsingHeader::new(title)
        .id_salt(title)
        .default_open(open)
        .show(ui, body)
        .body_returned
        .flatten()
}

fn fields(
    ui: &mut egui::Ui,
    index: &Index,
    row: ExcelRow<'_>,
    names: impl Iterator<Item = String>,
) -> Option<Action> {
    let mut action = None;
    for name in names {
        let Some(at) = index.column(&name) else {
            continue;
        };
        let Ok((_, column)) = index.table.get_column_by_offset(at) else {
            continue;
        };
        if blank(row, column) {
            continue;
        }
        let Ok(cell) = index.table.cell_by_offset(row, at) else {
            continue;
        };
        ui.horizontal(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.add_sized(
                [ui.available_width() * 0.45, ui.spacing().interact_size.y],
                Label::new(RichText::new(&name).weak()),
            );
            if let CellResponse::Link((sheet, (row_id, subrow))) = cell.show(ui).inner {
                action = Some(Action::Navigate(match subrow {
                    Some(subrow) => format!("/sheet/{sheet}#R{row_id}.{subrow}"),
                    None => format!("/sheet/{sheet}#R{row_id}"),
                }));
            }
        });
    }
    action
}

/// Rows the game leaves at zero or empty are slots the quest does not use.
fn blank(row: ExcelRow<'_>, column: &SheetColumnDefinition) -> bool {
    if column.kind() == ironworks::file::exh::ColumnKind::String {
        return row
            .read_string(u32::from(column.offset()))
            .is_ok_and(|value| value.as_bytes().is_empty());
    }
    crate::sheet::read_integer::<i64>(row, u32::from(column.offset()), column.kind())
        .is_ok_and(|value| value == 0)
}

fn quest_list(ui: &mut egui::Ui, index: &Index, nodes: &[u32]) -> Option<Action> {
    let mut action = None;
    for node in nodes {
        let quest = index.quest(*node);
        let response = ui
            .add(
                Label::new(RichText::new(&quest.name).color(ui.visuals().hyperlink_color))
                    .sense(Sense::click()),
            )
            .on_hover_text(&quest.id)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            action = Some(Action::Select(quest.row_id));
        }
    }
    action
}

fn asset_link(ui: &mut egui::Ui, path: &str) -> Option<Action> {
    let response = ui
        .add(
            Label::new(
                RichText::new(path)
                    .color(ui.visuals().hyperlink_color)
                    .small(),
            )
            .sense(Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response
        .clicked()
        .then(|| Action::Navigate(format!("/assets/{path}")))
}

/// Dialogue leans on more payload kinds than most sheets, so a player-name macro reads as a gap in
/// the sentence. That is the formatter having nothing to put there, not a decode failure.
fn sestring(ui: &egui::Ui, bytes: &[u8]) -> String {
    let text: &SeStr = bytes.into();
    if EVALUATE_STRINGS.get(ui.ctx()) {
        text.format()
            .try_to_compact_string()
            .map_or_else(|_| String::new(), Into::into)
    } else {
        text.macro_string()
            .try_to_compact_string()
            .map_or_else(|_| String::new(), Into::into)
    }
}

async fn dialogue(
    excel: CachedProvider,
    name: String,
    id_upper: String,
    language: Language,
) -> Result<Dialogue> {
    let sheet = excel.get_sheet(&name, language).await?;
    let columns = SheetColumnDefinition::from_sheet(&sheet);
    let (key, body) = match columns.as_slice() {
        [key, body, ..] => (key, body),
        _ => return Err(anyhow!("{name} is not a two column text sheet")),
    };

    let mut groups: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
    let mut lines = 0;
    for row_id in sheet.get_row_ids() {
        let Ok(row) = sheet.get_row(row_id) else {
            continue;
        };
        let (Ok(key), Ok(text)) = (
            row.read_string(u32::from(key.offset())),
            row.read_string(u32::from(body.offset())),
        ) else {
            continue;
        };
        if text.as_bytes().is_empty() {
            continue;
        }
        let key = String::from_utf8_lossy(key.as_bytes());
        let speaker = match derive::line_of(&key, &id_upper) {
            Line::Journal => "Journal".to_string(),
            Line::Objective => "Objectives".to_string(),
            Line::System => "System".to_string(),
            Line::Speaker(speaker) => speaker.to_string(),
        };
        lines += 1;
        match groups.iter_mut().find(|(name, _)| *name == speaker) {
            Some((_, texts)) => texts.push(text.as_bytes().to_vec()),
            None => groups.push((speaker, vec![text.as_bytes().to_vec()])),
        }
    }
    Ok(Dialogue { groups, lines })
}

async fn links(
    backend: Backend,
    language: Language,
    script: String,
    params: Vec<(Param, u32)>,
) -> Result<Links> {
    let mut music = Vec::new();
    let mut cutscenes = Vec::new();
    for (param, arg) in params {
        let name = match param {
            Param::Bgm => "BGM",
            Param::Cutscene => "Cutscene",
        };
        let sheet = backend.excel().get_sheet(name, language).await?;
        // Both sheets name their file in their first schema field, which is the first column in
        // offset order.
        let Some(column) = SheetColumnDefinition::from_sheet(&sheet).into_iter().next() else {
            continue;
        };
        let Ok(row) = sheet.get_row(arg) else {
            continue;
        };
        let Ok(value) = row.read_string(u32::from(column.offset())) else {
            continue;
        };
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        if value.is_empty() {
            continue;
        }
        match param {
            Param::Bgm => music.push(value),
            Param::Cutscene => cutscenes.push(derive::cutscene_path(&value)),
        }
    }
    music.sort_unstable();
    music.dedup();
    cutscenes.sort_unstable();
    cutscenes.dedup();

    // An instruction name can carry a cutscene id in a namespace of its own, so a link is only
    // offered for a file that is really there. A missing one means "not shown", not "absent".
    let present = backend.files().exists_many(&cutscenes).await?;
    let cutscenes = cutscenes
        .into_iter()
        .zip(present)
        .filter_map(|(path, exists)| exists.then_some(path))
        .collect();

    Ok(Links {
        script,
        music,
        cutscenes,
    })
}
