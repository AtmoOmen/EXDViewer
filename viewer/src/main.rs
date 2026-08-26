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

/// `viewer --smoke <sqpack-path> <out-dir> <step> [<step> ...]`, where a step is `model:<path>` or
/// `scene:<path>`. See `smoke/native.sh`.
#[cfg(not(target_arch = "wasm32"))]
fn smoke_config(mut args: std::iter::Skip<std::env::Args>) -> viewer::smoke::Config {
    let usage = "usage: viewer --smoke <sqpack> <out-dir> <step:path>...";
    let sqpack_path = args.next().expect(usage);
    let out_dir = args.next().expect(usage).into();
    let steps = args
        .map(|arg| {
            let (kind, path) = arg.split_once(':').expect("step must be kind:path");
            match kind {
                "model" => viewer::smoke::Step::Model(path.to_string()),
                "scene" => viewer::smoke::Step::Scene(path.to_string()),
                other => panic!("unknown step kind {other}"),
            }
        })
        .collect();
    viewer::smoke::Config {
        sqpack_path,
        schema_path: std::env::var("EXDVIEWER_SCHEMA_PATH").ok(),
        steps,
        out_dir,
        width: 1600,
        height: 1000,
        step_timeout: std::time::Duration::from_secs(60),
    }
}

// When compiling natively:
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--smoke") {
        let config = smoke_config(args);
        let (logger, counters) = viewer::smoke::CountingLogger::new(
            env_logger::Builder::from_env(env_logger::Env::new().default_filter_or("info")).build(),
        );
        logger.init();
        log::set_max_level(log::LevelFilter::Info);
        let smoke_options = eframe::NativeOptions {
            depth_buffer: 24,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([config.width as f32, config.height as f32])
                .with_visible(false),
            ..Default::default()
        };
        return eframe::run_native(
            "XIViewer (smoke)",
            smoke_options,
            Box::new(|cc| Ok(Box::new(viewer::smoke::SmokeApp::new(cc, config, counters)))),
        );
    }

    CombinedLogger(
        env_logger::Builder::from_env(env_logger::Env::new().default_filter_or("info")).build(),
        egui_logger::builder().build(),
    )
    .init();
    log::set_max_level(log::LevelFilter::Info);

    let native_options = eframe::NativeOptions {
        // A web canvas comes with depth already; glutin is asked for none unless this says so, and
        // the model viewer draws into whatever the window hands it.
        depth_buffer: 24,
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
        "XIViewer",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
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
                        for shortcut in &[GOTO_ROW, GOTO_SHEET, PALETTE] {
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
                    loading_text.set_inner_html(
                        "<p> The app has crashed. See the developer console for details. </p>",
                    );
                    panic!("Failed to start eframe: {e:?}");
                }
            }
        }
    });
}
