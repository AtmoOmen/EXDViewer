//! The export idiom every "Export" control in the app shares: a button when there is one way to
//! save a file, a dropdown when there are several, one spinner and one disabled state while a save
//! is running, and a file dialog once the bytes are ready.

use std::io::{Cursor, Write};

use anyhow::Result;
use egui::WidgetText;
use futures_util::future::LocalBoxFuture;
use zip::{ZipWriter, write::SimpleFileOptions};

use super::TrackedPromise;

/// A choice's own name for what it built, alongside the bytes to write.
type Named = Result<(String, Vec<u8>)>;

/// Bundles named files into a zip, for a choice that only makes sense once there is genuinely more
/// than one output file.
pub fn zip(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        archive.start_file(name, SimpleFileOptions::default())?;
        archive.write_all(data)?;
    }
    Ok(archive.finish()?.into_inner())
}

/// One way to save the file on show. `build` runs synchronously, at the moment this choice is
/// picked: a producer needing a live borrow (a model gathering its current scene, say) does that
/// half here, and hands back a future that owns everything the write and dialog need. Building a
/// `Choice` itself must stay cheap, since the caller does it every frame the menu could open; the
/// borrow in `build` is what keeps a raw file's bytes from being cloned before someone asks for
/// them.
///
/// The future resolves to the file's own name alongside its bytes rather than a fixed one, since a
/// producer that bundles more than one file (a zip, past the first) cannot always name itself
/// before it has gathered them.
pub struct Choice<'a> {
    label: String,
    hover: Option<String>,
    dialog_title: String,
    filter: Option<(String, Vec<String>)>,
    enabled: bool,
    disabled_hover: Option<String>,
    build: Box<dyn FnOnce() -> LocalBoxFuture<'static, Named> + 'a>,
}

impl<'a> Choice<'a> {
    /// The general form: `build` names the file it saves for itself.
    pub fn named(
        label: impl Into<String>,
        build: impl FnOnce() -> LocalBoxFuture<'static, Named> + 'a,
    ) -> Self {
        let label = label.into();
        Self {
            dialog_title: label.clone(),
            label,
            hover: None,
            filter: None,
            enabled: true,
            disabled_hover: None,
            build: Box::new(build),
        }
    }

    /// The common case: a fixed file name known up front.
    pub fn new(
        label: impl Into<String>,
        file_name: impl Into<String>,
        build: impl FnOnce() -> LocalBoxFuture<'static, Result<Vec<u8>>> + 'a,
    ) -> Self {
        let file_name = file_name.into();
        Self::named(label, move || {
            let inner = build();
            Box::pin(async move { Ok((file_name, inner.await?)) })
        })
    }

    /// A fixed file name, with nothing to await: a producer with only something to compute. Runs
    /// at the moment the choice is picked, same as `build` itself.
    pub fn bytes(
        label: impl Into<String>,
        file_name: impl Into<String>,
        produce: impl FnOnce() -> Result<Vec<u8>> + 'a,
    ) -> Self {
        Self::new(label, file_name, move || {
            let result = produce();
            Box::pin(async move { result })
        })
    }

    /// The synchronous form of [`Choice::named`]: `produce` names the file it hands back for
    /// itself, such as a bundle that is a lone file's own name until a second one makes it a zip.
    pub fn named_bytes(
        label: impl Into<String>,
        produce: impl FnOnce() -> Named + 'a,
    ) -> Self {
        Self::named(label, move || {
            let result = produce();
            Box::pin(async move { result })
        })
    }

    /// A file's own bytes, unchanged. Cloned only once this is picked, not while the menu merely
    /// carries the choice around.
    pub fn raw(bytes: &'a [u8], file_name: impl Into<String>) -> Self {
        Self::bytes("原始文件", file_name, move || Ok(bytes.to_vec()))
            .hover("文件保持原样存储的内容")
    }

    pub fn hover(mut self, text: impl Into<String>) -> Self {
        self.hover = Some(text.into());
        self
    }

    pub fn title(mut self, text: impl Into<String>) -> Self {
        self.dialog_title = text.into();
        self
    }

    pub fn filter(mut self, name: impl Into<String>, extensions: &[&str]) -> Self {
        self.filter = Some((
            name.into(),
            extensions.iter().map(|ext| (*ext).to_owned()).collect(),
        ));
        self
    }

    /// Disables the choice and says why, the way a disabled button elsewhere in the app explains
    /// itself with `on_disabled_hover_text`.
    pub fn unless(mut self, ready: bool, why: impl Into<String>) -> Self {
        if !ready {
            self.enabled = false;
            self.disabled_hover = Some(why.into());
        }
        self
    }
}

/// Where `menu` remembers the last failure for the control at this exact spot in the tree, so it
/// can show it without the caller wiring anything through its own promise: egui's own per-id temp
/// storage already survives across frames the way the promise it hands back does not.
fn error_slot(ui: &egui::Ui) -> egui::Id {
    ui.id().with("export-error")
}

/// Draws the control and, once a choice is picked, starts it: a plain button standing for the one
/// choice on offer, or `button` opening a menu of several (`hover` names it, for a button that is
/// only a glyph). Returns the promise a click started, for the caller to hold in its own
/// `Option<TrackedPromise<()>>` field; that field's own `take_if` is what clears it again; this
/// only ever returns `Some` on the frame a choice was picked, and it returns `None` if there is
/// nothing to export.
///
/// `min_size` matches the opener to controls drawn beside it with `add_sized`: `egui::Button` only
/// takes its height from ambient style (`interact_size.y`), never its width, so lining the two up
/// needs an explicit size here too. `Vec2::ZERO` leaves the button auto-sized, as every caller but
/// one wants.
///
/// A failed export shows here too, as a warning glyph with the reason on hover, so it is not only
/// `log::error!` that hears about it.
pub fn menu<'a>(
    ui: &mut egui::Ui,
    button: impl Into<WidgetText>,
    hover: Option<&str>,
    busy: bool,
    mut choices: Vec<Choice<'a>>,
    min_size: egui::Vec2,
) -> Option<TrackedPromise<()>> {
    if choices.is_empty() {
        return None;
    }
    if busy {
        ui.spinner();
    }
    let error_id = error_slot(ui);
    let mut spawned = None;
    ui.add_enabled_ui(!busy, |ui| {
        if choices.len() == 1 {
            let choice = choices.pop().expect("checked len() == 1 above");
            let mut response = ui.add_enabled(
                choice.enabled,
                egui::Button::new(&choice.label).min_size(min_size),
            );
            if let Some(text) = choice.hover.as_deref().or(hover) {
                response = response.on_hover_text(text);
            }
            if let Some(why) = &choice.disabled_hover {
                response = response.on_disabled_hover_text(why);
            }
            if response.clicked() {
                spawned = Some(start(choice, ui.ctx().clone(), error_id));
            }
        } else {
            let (response, _inner) = egui::containers::menu::MenuButton::from_button(
                egui::Button::new(button.into()).min_size(min_size),
            )
            .ui(ui, |ui| {
                for choice in choices {
                    let (enabled, item_hover, disabled_hover) = (
                        choice.enabled,
                        choice.hover.clone(),
                        choice.disabled_hover.clone(),
                    );
                    let mut response =
                        ui.add_enabled(enabled, egui::Button::new(choice.label.clone()));
                    if let Some(text) = &item_hover {
                        response = response.on_hover_text(text);
                    }
                    if let Some(why) = &disabled_hover {
                        response = response.on_disabled_hover_text(why);
                    }
                    if response.clicked() {
                        spawned = Some(start(choice, ui.ctx().clone(), error_id));
                        ui.close();
                    }
                }
            });
            if let Some(text) = hover {
                response.on_hover_text(text);
            }
        }
    });
    if let Some(message) = ui.data(|data| data.get_temp::<String>(error_id)) {
        ui.colored_label(egui::Color32::LIGHT_RED, "⚠")
            .on_hover_text(message);
    }
    spawned
}

fn start(choice: Choice<'_>, ctx: egui::Context, error_id: egui::Id) -> TrackedPromise<()> {
    let Choice {
        dialog_title,
        filter,
        build,
        ..
    } = choice;
    let future = build();
    TrackedPromise::spawn_local(async move {
        let (file_name, data) = match future.await {
            Ok(named) => named,
            Err(error) => {
                log::error!("导出 {dialog_title} 失败: {error:?}");
                ctx.memory_mut(|memory| memory.data.insert_temp(error_id, error.to_string()));
                ctx.request_repaint();
                return;
            }
        };
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title(dialog_title)
            .set_file_name(&file_name);
        if let Some((name, extensions)) = &filter {
            let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter(name, &extensions);
        }
        if let Some(file) = dialog.save_file().await {
            match file.write(&data).await {
                Ok(()) => {
                    log::info!("{file_name} 导出成功");
                    ctx.memory_mut(|memory| memory.data.remove_temp::<String>(error_id));
                }
                Err(error) => {
                    log::error!("写入 {file_name} 失败: {error}");
                    ctx.memory_mut(|memory| memory.data.insert_temp(error_id, error.to_string()));
                }
            }
            ctx.request_repaint();
        }
    })
}
