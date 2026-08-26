//! The export idiom every "Export" control in the app shares: a button when there is one way to
//! save a file, a dropdown when there are several, one spinner and one disabled state while a save
//! is running, and a file dialog once the bytes are ready.

use anyhow::Result;
use egui::WidgetText;
use futures_util::future::LocalBoxFuture;

use super::TrackedPromise;

/// One way to save the file on show. `build` runs synchronously, at the moment this choice is
/// picked: a producer needing a live borrow (a model gathering its current scene, say) does that
/// half here, and hands back a future that owns everything the write and dialog need. Building a
/// `Choice` itself must stay cheap, since the caller does it every frame the menu could open; the
/// borrow in `build` is what keeps a raw file's bytes from being cloned before someone asks for
/// them.
pub struct Choice<'a> {
    label: String,
    hover: Option<String>,
    dialog_title: String,
    file_name: String,
    filter: Option<(String, Vec<String>)>,
    enabled: bool,
    disabled_hover: Option<String>,
    build: Box<dyn FnOnce() -> LocalBoxFuture<'static, Result<Vec<u8>>> + 'a>,
}

impl<'a> Choice<'a> {
    pub fn new(
        label: impl Into<String>,
        file_name: impl Into<String>,
        build: impl FnOnce() -> LocalBoxFuture<'static, Result<Vec<u8>>> + 'a,
    ) -> Self {
        let label = label.into();
        Self {
            dialog_title: label.clone(),
            label,
            hover: None,
            file_name: file_name.into(),
            filter: None,
            enabled: true,
            disabled_hover: None,
            build: Box::new(build),
        }
    }

    /// The common case: a producer with nothing to await, only something to compute. Runs at the
    /// moment the choice is picked, same as `build` itself.
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

    /// A file's own bytes, unchanged. Cloned only once this is picked, not while the menu merely
    /// carries the choice around.
    pub fn raw(bytes: &'a [u8], file_name: impl Into<String>) -> Self {
        Self::bytes("Raw file", file_name, move || Ok(bytes.to_vec()))
            .hover("The file exactly as it is stored")
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

/// Draws the control and, once a choice is picked, starts it: a plain button standing for the one
/// choice on offer, or `button` opening a menu of several. Returns the promise a click started, for
/// the caller to hold in its own `Option<TrackedPromise<()>>` field; that field's own `take_if` is
/// what clears it again; this only ever returns `Some` on the frame a choice was picked, and it
/// returns `None` if there is nothing to export.
pub fn menu<'a>(
    ui: &mut egui::Ui,
    button: impl Into<WidgetText>,
    busy: bool,
    mut choices: Vec<Choice<'a>>,
) -> Option<TrackedPromise<()>> {
    if choices.is_empty() {
        return None;
    }
    if busy {
        ui.spinner();
    }
    let mut spawned = None;
    ui.add_enabled_ui(!busy, |ui| {
        if choices.len() == 1 {
            let choice = choices.pop().expect("checked len() == 1 above");
            let mut response = ui.add_enabled(choice.enabled, egui::Button::new(&choice.label));
            if let Some(hover) = &choice.hover {
                response = response.on_hover_text(hover);
            }
            if let Some(why) = &choice.disabled_hover {
                response = response.on_disabled_hover_text(why);
            }
            if response.clicked() {
                spawned = Some(start(choice));
            }
        } else {
            ui.menu_button(button, |ui| {
                for choice in choices {
                    let (enabled, hover, disabled_hover) =
                        (choice.enabled, choice.hover.clone(), choice.disabled_hover.clone());
                    let mut response =
                        ui.add_enabled(enabled, egui::Button::new(choice.label.clone()));
                    if let Some(hover) = &hover {
                        response = response.on_hover_text(hover);
                    }
                    if let Some(why) = &disabled_hover {
                        response = response.on_disabled_hover_text(why);
                    }
                    if response.clicked() {
                        spawned = Some(start(choice));
                        ui.close();
                    }
                }
            });
        }
    });
    spawned
}

fn start(choice: Choice<'_>) -> TrackedPromise<()> {
    let Choice {
        dialog_title,
        file_name,
        filter,
        build,
        ..
    } = choice;
    let future = build();
    TrackedPromise::spawn_local(async move {
        let data = match future.await {
            Ok(data) => data,
            Err(error) => {
                log::error!("Failed to export {file_name}: {error:?}");
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
                Ok(()) => log::info!("Exported {file_name} successfully"),
                Err(error) => log::error!("Failed to write {file_name}: {error}"),
            }
        }
    })
}
