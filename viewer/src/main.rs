#![allow(dead_code)]
#![warn(
    clippy::all,
    rust_2018_idioms,
    rust_2021_compatibility,
    rust_2024_compatibility
)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod combined_log;

#[cfg(target_arch = "wasm32")]
mod shortcuts;

use combined_log::CombinedLogger;
use viewer::App;

// When compiling natively:
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    velopack::VelopackApp::build().run();

    CombinedLogger(
        env_logger::Builder::from_env(env_logger::Env::new().default_filter_or("info")).build(),
        egui_logger::builder().build(),
    )
    .init();
    log::set_max_level(log::LevelFilter::Info);

    check_for_updates();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_min_inner_size([300.0, 220.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(&include_bytes!("../assets/icon.png")[..])
                    .expect("Failed to load icon"),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "EXDViewer",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn check_for_updates() {
    let source = velopack::sources::GithubSource::new(viewer::UPDATE_REPO_URL, None, false);
    let manager = match velopack::UpdateManager::new(source, None, None) {
        Ok(manager) => manager,
        Err(error) => {
            log::debug!("无法初始化 Velopack 更新管理器: {error}");
            return;
        }
    };

    match manager.check_for_updates() {
        Ok(velopack::UpdateCheck::UpdateAvailable(update)) => {
            log::info!("发现可用更新: {}", update.TargetFullRelease.Version);
            if let Err(error) = manager.download_updates(&update, None) {
                log::warn!("下载更新失败: {error}");
                return;
            }
            if let Err(error) = manager.apply_updates_and_restart(&*update) {
                log::warn!("应用更新失败: {error}");
            }
        }
        Ok(velopack::UpdateCheck::RemoteIsEmpty) => {
            log::warn!("GitHub 更新源未提供可用版本");
        }
        Ok(velopack::UpdateCheck::NoUpdateAvailable) => {
            log::info!("当前已是最新版本");
        }
        Err(error) => {
            log::warn!("检查更新失败: {error}");
        }
    }
}

// When compiling to web using trunk:
#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast as _;

    CombinedLogger(
        eframe::WebLogger::new(log::LevelFilter::Debug),
        egui_logger::builder().build(),
    )
    .init();
    log::set_max_level(log::LevelFilter::Info);

    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("No window")
            .document()
            .expect("No document");

        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("Failed to find the_canvas_id")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("the_canvas_id was not a HtmlCanvasElement");

        let runner = eframe::WebRunner::new();

        let start_result = async {
            runner
                .start(
                    canvas.clone(),
                    web_options,
                    Box::new(|cc| Ok(Box::new(App::new(cc)))),
                )
                .await?;

            // Override certain key handling to prevent browser defaults.
            runner.add_event_listener(
                &canvas,
                "keydown",
                move |event: web_sys::KeyboardEvent, _| {
                    #[allow(clippy::wildcard_imports)]
                    use crate::shortcuts::*;

                    // https://github.com/emilk/egui/blob/802d307e4a2835cf4cf184d1cc99bea525b0c959/crates/eframe/src/web/input.rs#L152
                    let modifiers = egui::Modifiers {
                        alt: event.alt_key(),
                        ctrl: event.ctrl_key(),
                        shift: event.shift_key(),
                        mac_cmd: event.meta_key(),
                        command: event.ctrl_key() || event.meta_key(),
                    };
                    let key = egui::Key::from_name(&event.key());
                    if let Some(key) = key {
                        for shortcut in &[GOTO_ROW, GOTO_SHEET] {
                            if modifiers.matches_logically(shortcut.modifiers)
                                && key == shortcut.logical_key
                            {
                                event.prevent_default(); // Prevent browser default
                            }
                        }
                    }
                },
            )
        }
        .await;

        // Remove the loading text and spinner:
        if let Some(loading_text) = document.get_element_by_id("loading_text") {
            match start_result {
                Ok(()) => {
                    loading_text.remove();
                }
                Err(e) => {
                    loading_text.set_inner_html("<p> 应用已崩溃 请查看开发者控制台获取详情 </p>");
                    panic!("Failed to start eframe: {e:?}");
                }
            }
        }
    });
}
