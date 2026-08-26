use egui::{RichText, Sense, Spinner, Vec2};

use crate::{
    data::get_icon_path,
    excel::provider::{ExcelProvider, ExcelRow, ExcelSheet},
    sheet::{
        GlobalContext,
        cell::CellValue,
        read_integer,
        schema_column::{ResolvedTableContext, SchemaColumnMeta, SheetLink},
        table_context::TableContext,
    },
    utils::{ManagedIcon, TrackedPromise},
};

/// Whether a `Link` targets the `EventIconType` sheet specifically, rather than any other link.
pub fn links_here(sheets: &SheetLink) -> bool {
    matches!(sheets.targets(), [name] if name == "EventIconType")
}

/// `EventIconType` names a quest-marker *type*: which icon shows over the NPC and on the map, for
/// the available and the unavailable state. The four `type: icon` columns are that mapping;
/// `IconRange` further widens each into a block of alternate icons the client picks from at
/// runtime (not decodable from this sheet alone, per a manual check of the actual pixels).
const SLOTS: [(&str, &str); 4] = [
    ("NpcIconAvailable", "NPC available"),
    ("MapIconAvailable", "Map available"),
    ("NpcIconInvalid", "NPC unavailable"),
    ("MapIconInvalid", "Map unavailable"),
];

pub enum Resolved {
    Pending,
    Empty,
    Icons(Vec<(&'static str, u32)>, i128),
}

/// Resolve the `EventIconType` column at offset index `at` on `row`, following its `Link` to the
/// `EventIconType` sheet and reading back the icon columns by name.
pub fn resolve(table: &TableContext, at: u32, row: ExcelRow<'_>) -> Resolved {
    let Ok((schema_column, sheet_column)) = table.get_column_by_offset(at) else {
        return Resolved::Empty;
    };
    let SchemaColumnMeta::Link(sheets) = schema_column.meta() else {
        return Resolved::Empty;
    };
    let Ok(row_id) = read_integer::<i128>(row, sheet_column.offset() as u32, sheet_column.kind())
    else {
        return Resolved::Empty;
    };
    let Ok(row_id) = u32::try_from(row_id) else {
        return Resolved::Empty;
    };
    if row_id == 0 {
        return Resolved::Empty;
    }

    match sheets.resolve(table, row_id) {
        ResolvedTableContext::InProgress => Resolved::Pending,
        ResolvedTableContext::NotFound => Resolved::Empty,
        ResolvedTableContext::Found { table: linked, .. } => {
            let target_row = linked
                .sheet()
                .get_row(row_id)
                .expect("resolve() only returns Found once the row is confirmed to exist");
            let Ok(columns) = linked.columns() else {
                return Resolved::Empty;
            };

            let column_of = |name: &str| columns.iter().position(|(c, _)| c.name() == name);

            let icons = SLOTS
                .into_iter()
                .filter_map(|(name, label)| {
                    let idx = column_of(name)?;
                    let cell = linked.cell_by_offset(target_row, idx as u32).ok()?;
                    match cell.read(false).ok()? {
                        CellValue::Icon(icon_id) => u32::try_from(icon_id)
                            .ok()
                            .filter(|id| *id != 0)
                            .map(|id| (label, id)),
                        _ => None,
                    }
                })
                .collect();

            let range = column_of("IconRange")
                .and_then(|idx| linked.cell_by_offset(target_row, idx as u32).ok())
                .and_then(|cell| cell.read(false).ok())
                .map_or(0, |value| match value {
                    CellValue::Integer(range) => range,
                    _ => 0,
                });

            Resolved::Icons(icons, range)
        }
    }
}

const THUMB: f32 = 32.0;

/// Fixed height a cell showing this column takes, so the row sizing pass avoids resolving the
/// link (and its cross-sheet load) to measure it.
pub fn cell_height(ui: &egui::Ui) -> f32 {
    THUMB + ui.spacing().item_spacing.y + ui.text_style_height(&egui::TextStyle::Small)
}

/// Draw whatever `resolve` came back with: nothing for an unset field, a spinner while the target
/// sheet loads, or the set of icons the row names. Returns the id of an icon the user clicked, if
/// any, so the caller can open it in the shared icon modal.
pub fn ui(ui: &mut egui::Ui, global: &GlobalContext, resolved: &Resolved) -> Option<u32> {
    match resolved {
        Resolved::Pending => {
            ui.spinner();
            None
        }
        Resolved::Empty => None,
        Resolved::Icons(icons, range) if !icons.is_empty() => {
            ui.horizontal(|ui| {
                icons.iter().fold(None, |clicked, (label, icon_id)| {
                    clicked.or(thumb(ui, global, *icon_id, label, *range))
                })
            })
            .inner
        }
        Resolved::Icons(..) => None,
    }
}

fn thumb(
    ui: &mut egui::Ui,
    global: &GlobalContext,
    icon_id: u32,
    label: &str,
    range: i128,
) -> Option<u32> {
    let path = get_icon_path(global.backend().icons(), icon_id, false, global.language());
    let excel = global.backend().excel().clone();
    let source = global
        .icon_manager()
        .get_or_insert_icon(&path, ui.ctx(), || {
            let path = path.clone();
            TrackedPromise::spawn_local(async move { excel.get_icon(&path).await })
        });

    ui.vertical(|ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(THUMB), Sense::click());
        match source {
            ManagedIcon::Loaded(image) => crate::icons::fit_into(ui, image, rect),
            ManagedIcon::Loading | ManagedIcon::NotLoaded => {
                Spinner::new().paint_at(ui, rect);
            }
            ManagedIcon::Failed(_) => {}
        }
        let clicked = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked();
        ui.label(RichText::new(label).weak().small())
            .on_hover_text(format!(
                "Id: {icon_id}\nPath: {path}\nIconRange: {range} (additional variants not shown)"
            ));
        clicked.then_some(icon_id)
    })
    .inner
}
