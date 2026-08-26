use std::rc::Rc;

use egui::{Button, Context, Id, Image, Layout, Modal, Sense, Spinner, TextStyle, UiBuilder, Vec2};

use super::{ManagedIcon, TrackedPromise, export, icon_export_choices, spawn_icon_copy};
use crate::{data::FileProvider, excel::base::CachedProvider};

/// Loading/failed states have no image to size against, so they hold this rect instead.
const PLACEHOLDER: f32 = 160.0;
/// A modal is for looking closer, not for turning a 40px icon into a poster; large source
/// textures still shrink to fit, small ones show at their own size.
const MAX_ZOOM: f32 = 512.0;

/// Show `icon` over the whole app, with copy/export buttons for it below. Returns true once the
/// modal has been dismissed.
pub fn icon_modal(
    ctx: &Context,
    icon_id: u32,
    icon: ManagedIcon,
    export: &mut Option<TrackedPromise<()>>,
    excel: CachedProvider,
    files: Rc<dyn FileProvider>,
    path: &str,
) -> bool {
    export.take_if(|promise| promise.try_get().is_some());
    let loaded_source = match &icon {
        ManagedIcon::Loaded(source) => Some(source.clone()),
        _ => None,
    };

    Modal::new(Id::new("icon-modal"))
        .area(Modal::default_area(Id::new(format!(
            "icon-modal-{icon_id}"
        ))))
        .show(ctx, |ui| {
            match icon {
                ManagedIcon::Loaded(icon) => {
                    ui.add(
                        Image::new(icon)
                            .maintain_aspect_ratio(true)
                            .fit_to_original_size(1.0)
                            .max_size(Vec2::splat(MAX_ZOOM)),
                    );
                }
                ManagedIcon::Failed(e) => {
                    ui.set_width(PLACEHOLDER);
                    ui.set_height(PLACEHOLDER);
                    ui.centered_and_justified(|ui| {
                        ui.label("Failed to load icon").on_hover_text(e.to_string())
                    });
                }
                ManagedIcon::Loading => {
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::splat(PLACEHOLDER), Sense::hover());
                    ui.scope_builder(
                        UiBuilder::new()
                            .max_rect(rect)
                            .layout(Layout::centered_and_justified(ui.layout().main_dir())),
                        |ui| {
                            ui.add(Spinner::new().size(
                                ui.text_style_height(&TextStyle::Heading) * 3.0,
                            ))
                        },
                    );
                }
                ManagedIcon::NotLoaded => {
                    ui.label("Icon not loaded");
                }
            }

            ui.add_space(4.0);
            let busy = export.is_some();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(loaded_source.is_some() && !busy, Button::new("Copy"))
                    .on_hover_text("Copy the icon to the clipboard")
                    .clicked()
                    && let Some(source) = loaded_source.clone()
                {
                    *export = Some(spawn_icon_copy(
                        ui.ctx(),
                        excel.clone(),
                        icon_id,
                        path.to_owned(),
                        source,
                    ));
                }
                if ui
                    .button("Copy Id")
                    .on_hover_text("Copy the icon's id to the clipboard")
                    .clicked()
                {
                    ui.ctx().copy_text(icon_id.to_string());
                }
                let choices =
                    icon_export_choices(ui.ctx(), excel, files, icon_id, path, loaded_source);
                let promise = export::menu(ui, "Export", None, busy, choices, egui::Vec2::ZERO);
                if promise.is_some() {
                    *export = promise;
                }
            });
        })
        .should_close()
}
