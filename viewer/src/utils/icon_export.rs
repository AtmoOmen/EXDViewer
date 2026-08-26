use std::rc::Rc;

use anyhow::Result;
use either::Either;
use image::RgbaImage;

use crate::{
    data::FileProvider,
    excel::{base::CachedProvider, provider::ExcelProvider},
};

use super::{IconManager, TrackedPromise, export};

/// A `Uri` source is a `.tex` file the web backend hands the browser a link to; only the loader
/// already showing it on screen (`icon_loader::TexLoader`) knows how to decode that, so this reads
/// back its cache rather than refetching and running the bytes through the wrong decoder. A
/// `Texture` source came from a decoded [`RgbaImage`] the manager uploaded and dropped, so getting
/// it back means asking the backend for the same icon again.
pub async fn resolve_icon_pixels(
    ctx: &egui::Context,
    excel: CachedProvider,
    path: &str,
    source: egui::ImageSource<'static>,
) -> Result<RgbaImage> {
    if let egui::ImageSource::Uri(uri) = source {
        return match ctx.try_load_image(&uri, egui::SizeHint::Scale(1.0.into())) {
            Ok(egui::load::ImagePoll::Ready { image }) => Ok(color_image_to_rgba(&image)),
            Ok(egui::load::ImagePoll::Pending { .. }) => {
                anyhow::bail!("icon is still loading")
            }
            Err(error) => Err(anyhow::anyhow!("{error}")),
        };
    }
    match excel.get_icon(path).await? {
        Either::Right(image) => Ok(image),
        Either::Left(_) => anyhow::bail!("expected a decoded icon, got a URL"),
    }
}

fn color_image_to_rgba(image: &egui::ColorImage) -> RgbaImage {
    let [width, height] = image.size;
    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    RgbaImage::from_raw(width as u32, height as u32, bytes)
        .expect("ColorImage's pixel buffer matches its own size")
}

/// Copy an icon to the clipboard. Returns the promise for the caller's own
/// `Option<TrackedPromise<()>>` slot; a promise dropped mid-flight cancels its future, so that slot
/// has to outlive the frame that started this.
pub fn spawn_icon_copy(
    ctx: &egui::Context,
    excel: CachedProvider,
    icon_id: u32,
    path: String,
    source: egui::ImageSource<'static>,
) -> TrackedPromise<()> {
    let ctx = ctx.clone();
    TrackedPromise::spawn_local(async move {
        match resolve_icon_pixels(&ctx, excel, &path, source).await {
            Ok(image) => ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            )),
            Err(error) => log::error!("Failed to resolve icon {icon_id} for export: {error}"),
        }
    })
}

/// The pixels, resolved and PNG-encoded.
fn icon_png_choice(
    ctx: &egui::Context,
    excel: CachedProvider,
    icon_id: u32,
    path: String,
    source: egui::ImageSource<'static>,
) -> export::Choice<'static> {
    let file_name = format!("icon_{icon_id:06}.png");
    let ctx = ctx.clone();
    export::Choice::new("Export PNG…", file_name, move || {
        Box::pin(async move {
            let image = resolve_icon_pixels(&ctx, excel, &path, source).await?;
            crate::utils::tex_loader::write(image, image::ImageFormat::Png)
        })
    })
    .title("Export Icon")
    .filter("PNG image", &["png"])
}

/// The file exactly as sqpack stores it, undecoded.
fn icon_raw_choice(files: Rc<dyn FileProvider>, icon_id: u32, path: String) -> export::Choice<'static> {
    let file_name = format!("icon_{icon_id:06}.tex");
    export::Choice::new(
        "Export Raw (.tex)…",
        file_name,
        move || Box::pin(async move { files.read(&path).await }),
    )
    .title("Export Icon")
    .filter("Texture", &["tex"])
}

/// Every way an icon can be exported: its resolved pixels as a PNG once it has decoded, and the raw
/// `.tex` file, which needs no decode and so is offered even while the preview is still loading.
pub fn icon_export_choices(
    ctx: &egui::Context,
    excel: CachedProvider,
    files: Rc<dyn FileProvider>,
    icon_id: u32,
    path: &str,
    source: Option<egui::ImageSource<'static>>,
) -> Vec<export::Choice<'static>> {
    let mut choices = Vec::new();
    if let Some(source) = source {
        choices.push(icon_png_choice(ctx, excel, icon_id, path.to_owned(), source));
    }
    choices.push(icon_raw_choice(files, icon_id, path.to_owned()));
    choices
}

/// The right-click menu every drawn icon offers, wherever it is drawn: copy the pixels, copy the
/// id, export to a file, and jump to it in the Icons tab. `icons` holds the promises this starts,
/// since most call sites have nowhere of their own to keep one alive. `source` is `None` while the
/// icon is still loading or failed to load, which still leaves the id and the Icons tab reachable.
pub fn icon_context_menu(
    response: &egui::Response,
    icons: &IconManager,
    excel: CachedProvider,
    files: Rc<dyn FileProvider>,
    icon_id: u32,
    path: &str,
    source: Option<egui::ImageSource<'static>>,
) {
    response.context_menu(|ui| {
        if ui
            .add_enabled(source.is_some(), egui::Button::new("Copy Image"))
            .clicked()
            && let Some(source) = source.clone()
        {
            icons.spawn_action(spawn_icon_copy(
                ui.ctx(),
                excel.clone(),
                icon_id,
                path.to_owned(),
                source,
            ));
            ui.close();
        }
        if ui.button("Copy Icon Id").clicked() {
            ui.ctx().copy_text(icon_id.to_string());
            ui.close();
        }
        let choices = icon_export_choices(ui.ctx(), excel, files, icon_id, path, source);
        let promise = export::menu(ui, "Export", None, false, choices);
        if let Some(promise) = promise {
            icons.spawn_action(promise);
        }
        ui.separator();
        if ui
            .button(format!("Open “{icon_id:06}” in Icons"))
            .clicked()
        {
            icons.request_open(icon_id);
            ui.close();
        }
    });
}
