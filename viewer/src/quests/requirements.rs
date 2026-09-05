use egui::RichText;

use crate::{
    excel::provider::ExcelRow,
    quests::{
        detail::{Action, link_action},
        glyph,
        index::Index,
        rewards::{ICON_SIZE, icon, read},
    },
};

/// The quest's requirements, resolved past raw ids where a linked sheet names or draws them: a
/// level paired with the class/job category it applies to, Grand Company rank, beast tribe
/// reputation (and its cap, where one applies), and a required mount. Follows the icon/name/link
/// idiom `rewards::ui` already established.
pub fn ui(ui: &mut egui::Ui, index: &Index, row: ExcelRow<'_>) -> Option<Action> {
    let mut action = class_row(ui, index, row, 0);
    action = action.or(class_row(ui, index, row, 1));
    action = action.or(company_row(ui, index, row));
    action = action.or(beast_row(ui, index, row));
    action = action.or(mount_row(ui, index, row));

    if read(index, row, "IsHouseRequired") != 0 {
        ui.label("需要房屋");
    }
    action
}

fn class_row(ui: &mut egui::Ui, index: &Index, row: ExcelRow<'_>, slot: usize) -> Option<Action> {
    let level = read(index, row, &format!("ClassJobLevel[{slot}]"));
    let category = format!("ClassJobCategory{slot}");
    let category_id = read(index, row, &category);
    if level == 0 && category_id == 0 {
        return None;
    }

    let mut action = None;
    ui.horizontal(|ui| {
        if level != 0 {
            ui.label(format!(
                "{} {level}",
                glyph::level(index.table.global().language())
            ));
        }
        if category_id != 0
            && let Some(at) = index.column(&category)
            && let Ok(cell) = index.table.cell_by_offset(row, at)
        {
            action = link_action(cell.show(ui).inner);
        }
    });
    action
}

fn company_row(ui: &mut egui::Ui, index: &Index, row: ExcelRow<'_>) -> Option<Action> {
    let company = read(index, row, "GrandCompany");
    let rank = read(index, row, "GrandCompanyRank");
    if company == 0 && rank == 0 {
        return None;
    }

    let mut action = None;
    ui.horizontal(|ui| {
        if rank != 0
            && let Some(at) = index.column("GrandCompanyRank")
            && let Ok(cell) = index.table.cell_by_offset(row, at)
        {
            action = link_action(cell.show(ui).inner);
        }
        if company != 0
            && let Some(at) = index.column("GrandCompany")
            && let Ok(cell) = index.table.cell_by_offset(row, at)
        {
            ui.label("位于");
            if let Some(new_action) = link_action(cell.show(ui).inner) {
                action = Some(new_action);
            }
        }
    });
    action
}

fn beast_row(ui: &mut egui::Ui, index: &Index, row: ExcelRow<'_>) -> Option<Action> {
    let tribe = read(index, row, "BeastTribe");
    if tribe == 0 {
        return None;
    }
    let tribe_at = index.column("BeastTribe")?;
    let icon_id = index.table.linked_icon(tribe_at, row, "Icon").unwrap_or(0);
    let rank = read(index, row, "BeastReputationRank");
    let cap = read(index, row, "BeastReputationValue");

    let mut action = None;
    ui.horizontal(|ui| {
        icon(ui, index, icon_id, ICON_SIZE);
        if let Ok(cell) = index.table.cell_by_offset(row, tribe_at) {
            action = link_action(cell.show(ui).inner);
        }
        if rank != 0
            && let Some(at) = index.column("BeastReputationRank")
            && let Ok(cell) = index.table.cell_by_offset(row, at)
        {
            ui.label("·");
            if let Some(new_action) = link_action(cell.show(ui).inner) {
                action = Some(new_action);
            }
        }
    });
    // 0xFFFF means no cap: see `blank()`'s comment on the same field.
    if cap != 0 && cap != 0xFFFF {
        ui.label(
            RichText::new(format!("所需声望上限为 {cap}"))
                .weak()
                .small(),
        );
    }
    action
}

fn mount_row(ui: &mut egui::Ui, index: &Index, row: ExcelRow<'_>) -> Option<Action> {
    let mount = read(index, row, "MountRequired");
    if mount == 0 {
        return None;
    }
    let at = index.column("MountRequired")?;
    let icon_id = index.table.linked_icon(at, row, "Icon").unwrap_or(0);

    let mut action = None;
    ui.horizontal(|ui| {
        icon(ui, index, icon_id, ICON_SIZE);
        if let Ok(cell) = index.table.cell_by_offset(row, at) {
            action = link_action(cell.show(ui).inner);
        }
    });
    action
}
