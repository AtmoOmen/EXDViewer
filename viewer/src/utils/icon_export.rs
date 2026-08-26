use anyhow::Result;
use either::Either;
use image::RgbaImage;

use crate::excel::{base::CachedProvider, provider::ExcelProvider};

use super::TrackedPromise;

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

/// Copy an icon to the clipboard, or encode it to PNG and hand it to a save dialog. Returns the
/// promise for the caller's own `Option<TrackedPromise<()>>` slot; a promise dropped mid-flight
/// cancels its future, so that slot has to outlive the frame that started this.
pub fn spawn_icon_export(
    ctx: &egui::Context,
    excel: CachedProvider,
    icon_id: u32,
    path: String,
    source: egui::ImageSource<'static>,
    to_file: bool,
) -> TrackedPromise<()> {
    let ctx = ctx.clone();
    TrackedPromise::spawn_local(async move {
        let image = match resolve_icon_pixels(&ctx, excel, &path, source).await {
            Ok(image) => image,
            Err(error) => {
                log::error!("Failed to resolve icon {icon_id} for export: {error}");
                return;
            }
        };
        if to_file {
            let data = match crate::utils::tex_loader::write(image, image::ImageFormat::Png) {
                Ok(data) => data,
                Err(error) => {
                    log::error!("Failed to encode icon {icon_id} as PNG: {error}");
                    return;
                }
            };
            if let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Export Icon")
                .set_file_name(format!("icon_{icon_id:06}.png"))
                .add_filter("PNG image", &["png"])
                .save_file()
                .await
            {
                if let Err(error) = file.write(&data).await {
                    log::error!("Failed to write icon {icon_id}: {error}");
                } else {
                    log::info!("Exported icon {icon_id} successfully");
                }
            }
        } else {
            ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            ));
        }
    })
}
