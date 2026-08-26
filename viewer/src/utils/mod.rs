mod cache;
mod cloneable_error;
mod collapsible_side_panel;
mod color_theme;
mod convertible_promise;
pub mod export;
mod icon_export;
mod icon_loader;
mod icon_manager;
mod icon_modal;
#[cfg(target_arch = "wasm32")]
mod jserror;
mod matcher;
mod opt_slider;
mod shared_future;
pub mod shortcut;
mod syntax_highlighting;
pub mod tex_loader;
mod tracked_promise;
mod unsend_promise;
mod version;
mod webreq;
mod yield_now;

pub use cache::KeyedCache;
pub use cloneable_error::CloneableResult;
pub use collapsible_side_panel::{CollapsibleSidePanel, Side};
pub use color_theme::ColorTheme;
pub use convertible_promise::{ConvertiblePromise, PromiseKind};
pub use icon_export::{resolve_icon_pixels, spawn_icon_export};
pub use icon_loader::install_tex_loader;
pub use icon_manager::{IconManager, ManagedIcon};
pub use icon_modal::icon_modal;
#[cfg(target_arch = "wasm32")]
pub use jserror::{JsErr, JsResult};
pub use matcher::FuzzyMatcher;
pub use opt_slider::opt_slider;
pub use shared_future::SharedFuture;
pub use syntax_highlighting::{CodeTheme, highlight};
pub use tracked_promise::{TrackedPromise, tick_promises};
pub use unsend_promise::UnsendPromise;
pub use version::GameVersion;
pub use webreq::{HttpResponse, fetch, fetch_range, fetch_url, fetch_url_str, request};
pub use yield_now::yield_to_ui;

/// The last segment of a game path. Paths are always slash-separated, so this is the file name.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Where a tab puts the placeholder it shows with nothing selected. Above the middle, so a short
/// column of text sits where the eye already is rather than at the centre of an empty pane.
pub fn center<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.35);
        contents(ui)
    })
    .inner
}

/// The glyph and line a tab shows with nothing selected.
pub fn empty_view(ui: &mut egui::Ui, glyph: &str, label: &str) {
    center(ui, |ui| {
        ui.label(egui::RichText::new(glyph).size(56.0).weak());
        ui.label(egui::RichText::new(label).weak());
    });
}
