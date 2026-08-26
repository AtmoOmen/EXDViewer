use egui::{Button, Context, Id, Image, Layout, Modal, Sense, Spinner, TextStyle, UiBuilder, Vec2};

use super::{ManagedIcon, TrackedPromise, spawn_icon_export};
use crate::excel::base::CachedProvider;

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
            let enabled = loaded_source.is_some() && export.is_none();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(enabled, Button::new("Copy"))
                    .on_hover_text("Copy the icon to the clipboard")
                    .clicked()
                    && let Some(source) = loaded_source.clone()
                {
                    *export = Some(spawn_icon_export(
                        ui.ctx(),
                        excel.clone(),
                        icon_id,
                        path.to_owned(),
                        source,
                        false,
                    ));
                }
                if ui
                    .add_enabled(enabled, Button::new("Export PNG…"))
                    .clicked()
                    && let Some(source) = loaded_source
                {
                    *export = Some(spawn_icon_export(
                        ui.ctx(),
                        excel,
                        icon_id,
                        path.to_owned(),
                        source,
                        true,
                    ));
                }
                if export.is_some() {
                    ui.spinner();
                }
            });
        })
        .should_close()
}
