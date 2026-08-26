use anyhow::Result;
use egui::{Color32, Label, RichText, Sense, Vec2};
use ironworks::{excel::Language, file::exh::ColumnKind};

use crate::{
    backend::Backend,
    data::get_icon_path,
    excel::provider::{ExcelProvider, ExcelRow, ExcelSheet},
    quests::{
        detail::Action,
        index::{Fields, Index, integer, text},
    },
    sheet::read_integer,
    settings::ALWAYS_HIRES,
    utils::{ManagedIcon, TrackedPromise, icon_context_menu},
};

/// How many ranks `BeastRankBonus.ItemQuantity` carries, from `Neutral` to `AlliedBloodsworn`.
const BEAST_RANKS: usize = 8;

/// The reward-adjacent sheets the panel resolves ids against, loaded once per language rather than
/// per quest.
pub struct Catalog {
    item: Fields,
    stain: Fields,
    emote: Fields,
    action: Fields,
    general_action: Fields,
    other: Fields,
    tomestones_item: Fields,
    class_job_reward: Fields,
    beast_rank_bonus: Fields,
}

impl Catalog {
    pub async fn load(backend: Backend, language: Language) -> Result<Self> {
        Ok(Self {
            item: Fields::load(&backend, "Item", language).await?,
            stain: Fields::load(&backend, "Stain", language).await?,
            emote: Fields::load(&backend, "Emote", language).await?,
            action: Fields::load(&backend, "Action", language).await?,
            general_action: Fields::load(&backend, "GeneralAction", language).await?,
            other: Fields::load(&backend, "QuestRewardOther", language).await?,
            tomestones_item: Fields::load(&backend, "TomestonesItem", language).await?,
            class_job_reward: Fields::load(&backend, "QuestClassJobReward", language).await?,
            beast_rank_bonus: Fields::load(&backend, "BeastRankBonus", language).await?,
        })
    }

    /// The `Item` a `Tomestones` id currently names, via `TomestonesItem`'s reverse link. Retired
    /// tomestone generations carry `Tomestones = 0` and are not reachable this way.
    fn tomestone_item(&self, tomestone_id: i64) -> Option<u32> {
        let item_column = self.tomestones_item.at("Item").ok()?;
        let tomestones_column = self.tomestones_item.at("Tomestones").ok()?;
        self.tomestones_item.sheet.get_row_ids().find_map(|row_id| {
            let row = self.tomestones_item.sheet.get_row(row_id).ok()?;
            (i64::from(integer(row, tomestones_column)) == tomestone_id)
                .then(|| integer(row, item_column))
        })
    }

    /// The job-specific reward items a `QuestClassJobReward` row offers, each with its own amount.
    /// The row also carries a required-item swap-in per slot that this does not model.
    fn class_job_reward(&self, row_id: u32) -> Vec<(u32, u32)> {
        let Ok(row) = self.class_job_reward.sheet.get_row(row_id) else {
            return Vec::new();
        };
        (0..4)
            .filter_map(|slot| {
                let item_column = self.class_job_reward.at(&format!("RewardItem[{slot}]")).ok()?;
                let item = integer(row, item_column);
                if item == 0 {
                    return None;
                }
                let amount_column = self.class_job_reward.at(&format!("RewardAmount[{slot}]")).ok()?;
                Some((item, integer(row, amount_column).max(1)))
            })
            .collect()
    }

    /// A beast tribe rank bonus's item and the quantity range it pays across ranks. The player's
    /// current rank is not something this panel knows, so only the range is honest to show.
    fn beast_rank_bonus(&self, row_id: u32) -> Option<(u32, u16, u16)> {
        let row = self.beast_rank_bonus.sheet.get_row(row_id).ok()?;
        let item = integer(row, self.beast_rank_bonus.at("Item").ok()?);
        let quantities: Vec<u16> = (0..BEAST_RANKS)
            .filter_map(|rank| {
                let column = self.beast_rank_bonus.at(&format!("ItemQuantity[{rank}]")).ok()?;
                Some(integer(row, column) as u16)
            })
            .collect();
        let min = *quantities.iter().min()?;
        let max = *quantities.iter().max()?;
        Some((item, min, max))
    }
}

fn name_icon(fields: &Fields, row_id: u32) -> Option<(String, u32)> {
    let row = fields.sheet.get_row(row_id).ok()?;
    let name = text(row, fields.at("Name").ok()?);
    let icon = integer(row, fields.at("Icon").ok()?);
    Some((name, icon))
}

/// `Stain.Color` packs a plain 24-bit RGB into the low three bytes with the high byte always zero,
/// not the RGBA the generic `type: color` cell renderer assumes - that reads the high byte as alpha
/// and renders every stain nearly transparent.
fn stain_color(fields: &Fields, row_id: u32) -> Option<(String, Color32)> {
    let row = fields.sheet.get_row(row_id).ok()?;
    let name = text(row, fields.at("Name").ok()?);
    let column = fields.at("Color").ok()?;
    let raw: u32 = read_integer(row, u32::from(column.offset()), column.kind()).ok()?;
    let [_, r, g, b] = raw.to_be_bytes();
    Some((name, Color32::from_rgb(r, g, b)))
}

/// `read_integer` only handles the integer `ColumnKind`s; the HQ flags in this sheet are plain
/// `Bool` columns, which it errors on and this would silently read as 0 without the special case.
fn read(index: &Index, row: ExcelRow<'_>, name: &str) -> i64 {
    let Some(at) = index.column(name) else {
        return 0;
    };
    let Ok((_, column)) = index.table.get_column_by_offset(at) else {
        return 0;
    };
    let offset = u32::from(column.offset());
    if column.kind() == ColumnKind::Bool {
        return i64::from(row.read_bool(offset).unwrap_or(false));
    }
    read_integer::<i64>(row, offset, column.kind()).unwrap_or(0)
}

pub(crate) fn icon(ui: &mut egui::Ui, index: &Index, icon_id: u32, size: f32) {
    if icon_id == 0 {
        ui.add_space(size);
        return;
    }
    let global = index.table.global();
    let icon_mgr = global.icon_manager();
    let hires = ALWAYS_HIRES.get(ui.ctx());
    let path = get_icon_path(global.backend().icons(), icon_id, hires, global.language());
    let excel = global.backend().excel().clone();
    let source = icon_mgr.get_or_insert_icon(&path, ui.ctx(), || {
        let excel = excel.clone();
        let path = path.clone();
        TrackedPromise::spawn_local(async move { excel.get_icon(&path).await })
    });
    let loaded = match &source {
        ManagedIcon::Loaded(image) => Some(image.clone()),
        _ => None,
    };
    let response = match source {
        ManagedIcon::Loaded(image) => {
            ui.add(egui::Image::new(image).sense(Sense::click()).fit_to_exact_size(Vec2::splat(size)))
        }
        ManagedIcon::Loading | ManagedIcon::NotLoaded => {
            ui.add(egui::Spinner::new().size(size))
        }
        ManagedIcon::Failed(_) => {
            let (_, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
            response
        }
    };
    icon_context_menu(&response, icon_mgr, excel, icon_id, &path, loaded);
}

/// A swatch constrained to its own small rect - `draw_color` otherwise claims the rest of the row.
fn swatch(ui: &mut egui::Ui, color: Color32, size: f32) -> egui::Response {
    ui.allocate_ui(Vec2::splat(size), |ui| crate::sheet::draw_color(ui, color))
        .inner
}

fn link_label(ui: &mut egui::Ui, label: &str, width: f32, sheet: &str, row_id: u32) -> Option<Action> {
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    let response = ui
        .add_sized(
            [width, ui.spacing().interact_size.y],
            Label::new(RichText::new(label).color(ui.visuals().hyperlink_color)).sense(Sense::click()),
        )
        .on_hover_text(label)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.clicked().then(|| Action::Navigate(format!("/sheet/{sheet}#R{row_id}")))
}

const ICON_SIZE: f32 = 20.0;
/// Space reserved for the count, stain swatch and HQ badge trailing an item row, so the name gets
/// the rest of the line instead of a fixed split that clips long names.
const TRAILING_WIDTH: f32 = 96.0;

#[allow(clippy::too_many_arguments)]
fn item_row(
    ui: &mut egui::Ui,
    index: &Index,
    catalog: &Catalog,
    item_id: u32,
    count: u32,
    stain_id: u32,
    hq: bool,
) -> Option<Action> {
    let mut action = None;
    let resolved = name_icon(&catalog.item, item_id);
    let icon_id = resolved.as_ref().map_or(0, |(_, icon_id)| *icon_id);
    let name = resolved.map_or_else(|| format!("Item #{item_id}"), |(name, _)| name);
    ui.horizontal(|ui| {
        icon(ui, index, icon_id, ICON_SIZE);
        let name_width = (ui.available_width() - TRAILING_WIDTH).max(40.0);
        action = link_label(ui, &name, name_width, "Item", item_id);
        if count > 1 {
            ui.label(format!("×{count}"));
        }
        if stain_id != 0
            && let Some((stain_name, color)) = stain_color(&catalog.stain, stain_id)
        {
            swatch(ui, color, 14.0).on_hover_text(stain_name);
        }
        if hq {
            ui.label(RichText::new("HQ").strong().small());
        }
    });
    action
}

fn ability_row(
    ui: &mut egui::Ui,
    index: &Index,
    fields: &Fields,
    sheet_name: &str,
    row_id: u32,
) -> Option<Action> {
    let (name, icon_id) = name_icon(fields, row_id)?;
    let mut action = None;
    ui.horizontal(|ui| {
        icon(ui, index, icon_id, ICON_SIZE);
        let width = (ui.available_width() - 8.0).max(40.0);
        action = link_label(ui, &name, width, sheet_name, row_id);
    });
    action
}

/// The quest's reward block, resolved to the items/currencies/abilities it actually names. Fields
/// with no known resolution (`GCTypeReward`, `SystemReward`) are left to the caller's generic
/// fallback.
pub fn ui(ui: &mut egui::Ui, index: &Index, row: ExcelRow<'_>, catalog: &Catalog) -> Option<Action> {
    let mut action = None;

    let gil = read(index, row, "GilReward");
    if gil != 0 {
        action = action.or(item_row(ui, index, catalog, 1, gil as u32, 0, false));
    }

    let currency = read(index, row, "CurrencyReward");
    if currency != 0 {
        let count = read(index, row, "CurrencyRewardCount").max(1);
        action = action.or(item_row(ui, index, catalog, currency as u32, count as u32, 0, false));
    }

    let reward_type = read(index, row, "ItemRewardType");
    for slot in 0..7 {
        let item = read(index, row, &format!("Reward[{slot}]"));
        if item == 0 {
            continue;
        }
        match reward_type {
            1 | 3 | 5 => {
                let count = read(index, row, &format!("ItemCountReward[{slot}]")).max(1);
                let stain = read(index, row, &format!("RewardStain[{slot}]"));
                let hq = read(index, row, &format!("Unknown{slot}")) != 0;
                action = action.or(item_row(ui, index, catalog, item as u32, count as u32, stain as u32, hq));
            }
            6 => {
                for (reward_item, amount) in catalog.class_job_reward(item as u32) {
                    action = action.or(item_row(ui, index, catalog, reward_item, amount, 0, false));
                }
            }
            7 => {
                if let Some((item_id, min, max)) = catalog.beast_rank_bonus(item as u32) {
                    let count = if min == max { min } else { max };
                    action = action.or(item_row(ui, index, catalog, item_id, u32::from(count), 0, false));
                    if min != max {
                        ui.label(
                            RichText::new(format!("{min}-{max} depending on beast tribe rank"))
                                .weak()
                                .small(),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let optional: Vec<u32> = (0..5)
        .filter(|slot| read(index, row, &format!("OptionalItemReward[{slot}]")) != 0)
        .collect();
    if !optional.is_empty() {
        ui.label(RichText::new("Choose one").weak().small());
        for slot in optional {
            let item = read(index, row, &format!("OptionalItemReward[{slot}]"));
            let count = read(index, row, &format!("OptionalItemCountReward[{slot}]")).max(1);
            let stain = read(index, row, &format!("OptionalItemStainReward[{slot}]"));
            let hq = read(index, row, &format!("OptionalItemIsHQReward[{slot}]")) != 0;
            action = action.or(item_row(ui, index, catalog, item as u32, count as u32, stain as u32, hq));
        }
    }

    let catalysts: Vec<u32> = (0..3)
        .filter(|slot| read(index, row, &format!("ItemCatalyst[{slot}]")) != 0)
        .collect();
    if !catalysts.is_empty() {
        ui.label(RichText::new("Catalyst").weak().small());
        for slot in catalysts {
            let item = read(index, row, &format!("ItemCatalyst[{slot}]"));
            let count = read(index, row, &format!("ItemCountCatalyst[{slot}]")).max(1);
            action = action.or(item_row(ui, index, catalog, item as u32, count as u32, 0, false));
        }
    }

    let emote = read(index, row, "EmoteReward");
    if emote != 0 {
        action = action.or(ability_row(ui, index, &catalog.emote, "Emote", emote as u32));
    }
    let action_reward = read(index, row, "ActionReward");
    if action_reward != 0 {
        action = action.or(ability_row(ui, index, &catalog.action, "Action", action_reward as u32));
    }
    for slot in 0..2 {
        let general = read(index, row, &format!("GeneralActionReward[{slot}]"));
        if general != 0 {
            action = action.or(ability_row(
                ui,
                index,
                &catalog.general_action,
                "GeneralAction",
                general as u32,
            ));
        }
    }
    let other = read(index, row, "OtherReward");
    if other != 0 {
        action = action.or(ability_row(ui, index, &catalog.other, "QuestRewardOther", other as u32));
    }
    let other_display = read(index, row, "QuestRewardOtherDisplay");
    if other_display != 0 {
        action = action.or(ability_row(
            ui,
            index,
            &catalog.other,
            "QuestRewardOther",
            other_display as u32,
        ));
    }

    let tomestone_reward = read(index, row, "TomestoneReward");
    if tomestone_reward != 0
        && let Some(item_id) = catalog.tomestone_item(tomestone_reward)
    {
        let count = read(index, row, "TomestoneCountReward").max(1);
        action = action.or(item_row(ui, index, catalog, item_id, count as u32, 0, false));
    }

    let reputation = read(index, row, "ReputationReward");
    if reputation != 0 {
        ui.label(format!("+{reputation} reputation"));
    }

    let exp_factor = read(index, row, "ExpFactor");
    if exp_factor != 0 {
        ui.label(format!("Experience ×{:.2}", exp_factor as f64 / 100.0))
            .on_hover_text(format!("ExpFactor {exp_factor}"));
    }

    action
}
