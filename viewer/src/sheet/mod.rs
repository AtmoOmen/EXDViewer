mod cell;
mod cell_iter;
mod compact_sestring;
mod csv;
mod event_icon_type;
mod filter;
mod global_context;
mod schema_column;
mod sheet_column;
mod sheet_table;
mod table_context;

use std::{
    cell::{Cell, RefCell},
    fmt::Write,
    sync::Arc,
};

use base64::{Engine, prelude::BASE64_STANDARD};
#[cfg(not(target_arch = "wasm32"))]
pub use cell::CellValue;
pub(crate) use cell::{draw_color, read_integer};
pub use cell::{CellResponse, MatchOptions};
use compact_str::ToCompactString;
pub use csv::export_csv;
use egui::{
    Align, Color32, Direction, FontSelection, Galley, Label, Layout, Response, RichText, Sense,
    text::LayoutJob,
};
#[cfg(not(target_arch = "wasm32"))]
pub use filter::CompiledFilterInput;
pub use filter::{ComplexFilter, FilterInput, FilterInputType, draw_guide as draw_filter_guide};
pub use global_context::GlobalContext;
use intmap::IntMap;
use ironworks::sestring::SeStr;
pub(crate) use schema_column::{SchemaColumn, SchemaColumnMeta};
pub(crate) use sheet_column::SheetColumnDefinition;
pub use sheet_table::{SheetTable, SheetTableResponse};
pub use table_context::TableContext;

use crate::{
    settings::{EVALUATE_STRINGS, TEXT_MAX_LINES, TEXT_USE_SCROLL, TEXT_WRAP_WIDTH},
    stopwatch::stopwatches::{MULTILINE3_STOPWATCH, MULTILINE4_STOPWATCH},
};

fn copyable_label(ui: &mut egui::Ui, text: &impl ToString) -> Response {
    ui.with_layout(
        Layout::centered_and_justified(Direction::LeftToRight).with_main_align(Align::Min),
        |ui| {
            let text = text.to_string();
            let resp = ui.add(Label::new(&text).sense(Sense::click()));
            resp.context_menu(|ui| {
                if ui.button("复制").clicked() {
                    ui.ctx().copy_text(text);
                    ui.close();
                }
            });
            resp
        },
    )
    .inner
}

fn string_label_wrapped(ui: &mut egui::Ui, value: &SeStr) -> Response {
    let text = if EVALUATE_STRINGS.get(ui.ctx()) {
        value
            .format()
            .try_to_compact_string()
            .map_err(|e| anyhow::anyhow!(e))
    } else {
        value
            .macro_string()
            .try_to_compact_string()
            .map_err(|e| anyhow::anyhow!(e))
    };

    let text = match text {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to format string: {e:?}");
            let resp = ui
                .with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        Label::new(RichText::new("⚠").color(Color32::LIGHT_RED)).selectable(false),
                    )
                    .on_hover_text(e.to_string())
                })
                .inner;
            return resp;
        }
    };

    let (line_count, galley) = wrap_string_lines_galley(ui, text.to_string());
    let resp = ui
        .with_layout(Layout::left_to_right(Align::Center), |ui| {
            if TEXT_USE_SCROLL.get(ui.ctx())
                && let Some(max_lines) = TEXT_MAX_LINES.get(ui.ctx())
                && line_count > max_lines.get().into()
            {
                let max_height =
                    ui.text_style_height(&egui::TextStyle::Body) * f32::from(max_lines.get());
                ui.style_mut().spacing.item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .max_height(max_height)
                    .min_scrolled_height(max_height)
                    .show(ui, |ui| ui.label(galley))
                    .inner
            } else {
                ui.label(galley)
            }
        })
        .inner;

    resp.context_menu(|ui| {
        if ui.button("复制").clicked() {
            ui.ctx().copy_text(text.into());
            ui.close();
        }
        if ui.button("复制原始数据 (Base64)").clicked() {
            ui.ctx().copy_text(BASE64_STANDARD.encode(value.as_bytes()));
            ui.close();
        }
        if ui.button("复制原始数据 (HEX)").clicked() {
            ui.ctx().copy_text(
                value
                    .as_bytes()
                    .iter()
                    .fold(String::new(), |mut output, b| {
                        let _ = write!(output, "{b:02X}");
                        output
                    }),
            );
            ui.close();
        }
    });

    resp
}

thread_local! {
    // Ambient rather than a parameter: the path into `create_galley` runs through `Cell::show`,
    // a signature every table cell shares.
    static WRAP_TO_PANEL_WIDTH: Cell<bool> = const { Cell::new(false) };
}

/// Wraps sheet cells rendered inside `f` to the panel's own available width instead of the fixed
/// `TEXT_WRAP_WIDTH` setting.
pub fn wrap_to_panel_width<R>(f: impl FnOnce() -> R) -> R {
    let previous = WRAP_TO_PANEL_WIDTH.with(|c| c.replace(true));
    let result = f();
    WRAP_TO_PANEL_WIDTH.with(|c| c.set(previous));
    result
}

/// Whether the caller is inside [`wrap_to_panel_width`], for the widgets a galley cannot speak for.
pub(crate) fn wrapping_to_panel() -> bool {
    WRAP_TO_PANEL_WIDTH.with(Cell::get)
}

fn create_galley(ui: &egui::Ui, text: String, try_elide: bool) -> Arc<Galley> {
    let setting_width = TEXT_WRAP_WIDTH
        .get(ui.ctx())
        .map_or(f32::INFINITY, |w| w.get().into());
    let constrained = WRAP_TO_PANEL_WIDTH.with(Cell::get);
    let max_width = if constrained {
        setting_width.min(ui.available_width())
    } else {
        setting_width
    };
    let mut layout = LayoutJob::simple(
        text,
        FontSelection::default().resolve(ui.style()),
        Color32::PLACEHOLDER,
        max_width,
    );
    if constrained {
        layout.wrap.break_anywhere = true;
    }
    if try_elide && let Some(max_lines) = TEXT_MAX_LINES.get(ui.ctx()) {
        layout.wrap.max_rows = max_lines.get().into();
        if max_lines.get() == 1 {
            layout.wrap.break_anywhere = true;
        }
    }

    let _sw = MULTILINE3_STOPWATCH.start();
    ui.fonts_mut(|fonts| fonts.layout_job(layout))
}

fn wrap_string_lines_galley(ui: &egui::Ui, text: String) -> (usize, Arc<Galley>) {
    let galley = create_galley(ui, text, !TEXT_USE_SCROLL.get(ui.ctx()));
    (galley.rows.len(), galley)
}

thread_local! {
    static ESTIMATE_LUT: RefCell<IntMap<u32, f32>> = const { RefCell::new(IntMap::new()) };
}

fn estimated_char_width(ui: &egui::Ui, lut: &mut IntMap<u32, f32>, ch: char) -> f32 {
    if let Some(width) = lut.get(ch.into()) {
        *width
    } else {
        let width =
            ui.fonts_mut(|f| f.glyph_width(&FontSelection::default().resolve(ui.style()), ch));
        lut.insert(ch.into(), width);
        width
    }
}

/// Wraps the string to fit within a maximum width, returning line count.
fn wrap_string_lines_estimate(ui: &egui::Ui, text: &str) -> usize {
    let _sw = MULTILINE4_STOPWATCH.start();

    if text.is_empty() {
        return 1;
    }

    let Some(max_width) = TEXT_WRAP_WIDTH.get(ui.ctx()).map(|f| f.get() as f32) else {
        return text.lines().count();
    };

    ESTIMATE_LUT.with_borrow_mut(|lut| {
        text.lines()
            .map(|line| {
                let mut line_count = 1;
                let mut current_width = 0.0;
                for char in line.chars() {
                    let char_width = estimated_char_width(ui, lut, char);
                    current_width += char_width;
                    if current_width > max_width {
                        line_count += 1;
                        current_width = char_width;
                    }
                }
                line_count
            })
            .sum()
    })
}

fn should_ignore_clicks(ui: &egui::Ui) -> bool {
    ui.input(|i| i.modifiers.matches_logically(egui::Modifiers::ALT))
}
