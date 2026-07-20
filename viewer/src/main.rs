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

#[cfg(not(target_arch = "wasm32"))]
use std::{
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

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
        Box::new(|cc| Ok(Box::new(StartupApp::new(cc)))),
    )
}

#[cfg(not(target_arch = "wasm32"))]
const MIN_CHECKING_DURATION: Duration = Duration::from_millis(500);
#[cfg(not(target_arch = "wasm32"))]
const STARTING_DURATION: Duration = Duration::from_millis(250);

#[cfg(not(target_arch = "wasm32"))]
enum UpdateEvent {
    Downloading(i16),
    Starting,
}

#[cfg(not(target_arch = "wasm32"))]
enum StartupPhase {
    Checking,
    Downloading(i16),
    Starting,
}

#[cfg(not(target_arch = "wasm32"))]
struct StartupApp {
    app: App,
    update_events: Receiver<UpdateEvent>,
    phase: StartupPhase,
    phase_started: Instant,
    pending_event: Option<UpdateEvent>,
    app_visible: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl StartupApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let app = App::new(cc);
        let (event_tx, update_events) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || run_update_check(event_tx, ctx));

        Self {
            app,
            update_events,
            phase: StartupPhase::Checking,
            phase_started: Instant::now(),
            pending_event: None,
            app_visible: false,
        }
    }

    fn process_update_events(&mut self) {
        while let Ok(event) = self.update_events.try_recv() {
            if matches!(&self.phase, StartupPhase::Checking)
                && self.phase_started.elapsed() < MIN_CHECKING_DURATION
            {
                self.pending_event = Some(event);
            } else {
                self.set_phase(event);
            }
        }

        if matches!(&self.phase, StartupPhase::Checking)
            && self.phase_started.elapsed() >= MIN_CHECKING_DURATION
            && let Some(event) = self.pending_event.take()
        {
            self.set_phase(event);
        }
    }

    fn set_phase(&mut self, event: UpdateEvent) {
        self.phase = match event {
            UpdateEvent::Downloading(progress) => StartupPhase::Downloading(progress.clamp(0, 100)),
            UpdateEvent::Starting => StartupPhase::Starting,
        };
        self.phase_started = Instant::now();
    }

    fn draw_startup(&self, ui: &mut egui::Ui) {
        let (title, detail, progress) = match self.phase {
            StartupPhase::Checking => ("正在检查更新", "正在连接 GitHub 更新源", None),
            StartupPhase::Downloading(progress) => {
                ("正在下载更新", "正在准备新版本", Some(progress))
            }
            StartupPhase::Starting => ("正在启动 EXDViewer", "即将进入主界面", None),
        };

        ui.vertical_centered(|ui| {
            ui.add_space(36.0);
            ui.add(
                egui::Image::new(egui::include_image!("../assets/icon-small.png"))
                    .fit_to_exact_size(egui::vec2(72.0, 72.0)),
            );
            ui.add_space(18.0);
            ui.label(egui::RichText::new(title).size(18.0).strong());
            ui.add_space(6.0);
            ui.label(egui::RichText::new(detail).weak());
            ui.add_space(18.0);

            if let Some(progress) = progress {
                ui.add(
                    egui::ProgressBar::new(f32::from(progress) / 100.0)
                        .desired_width(240.0)
                        .show_percentage(),
                );
            } else {
                ui.add(egui::Spinner::new().size(22.0));
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl eframe::App for StartupApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.app_visible {
            <App as eframe::App>::logic(&mut self.app, ctx, frame);
            return;
        }

        self.process_update_events();
        if matches!(&self.phase, StartupPhase::Starting)
            && self.phase_started.elapsed() >= STARTING_DURATION
        {
            self.app_visible = true;
            <App as eframe::App>::logic(&mut self.app, ctx, frame);
            return;
        }

        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.app_visible {
            <App as eframe::App>::ui(&mut self.app, ui, frame);
        } else {
            self.draw_startup(ui);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_update_check(event_tx: Sender<UpdateEvent>, ctx: egui::Context) {
    let source = velopack::sources::GithubSource::new(viewer::UPDATE_REPO_URL, None, false);
    let manager = match velopack::UpdateManager::new(source, None, None) {
        Ok(manager) => manager,
        Err(error) => {
            log::debug!("无法初始化 Velopack 更新管理器: {error}");
            send_update_event(&event_tx, &ctx, UpdateEvent::Starting);
            return;
        }
    };

    match manager.check_for_updates() {
        Ok(velopack::UpdateCheck::UpdateAvailable(update)) => {
            log::info!("发现可用更新: {}", update.TargetFullRelease.Version);
            send_update_event(&event_tx, &ctx, UpdateEvent::Downloading(0));

            let (progress_tx, progress_rx) = mpsc::channel();
            let progress_events = event_tx.clone();
            let progress_ctx = ctx.clone();
            let progress_thread = std::thread::spawn(move || {
                while let Ok(progress) = progress_rx.recv() {
                    send_update_event(
                        &progress_events,
                        &progress_ctx,
                        UpdateEvent::Downloading(progress),
                    );
                }
            });

            let result = manager.download_updates(&update, Some(progress_tx));
            let _ = progress_thread.join();

            if let Err(error) = result {
                log::warn!("下载更新失败: {error}");
                send_update_event(&event_tx, &ctx, UpdateEvent::Starting);
                return;
            }
            if let Err(error) = manager.apply_updates_and_restart(&*update) {
                log::warn!("应用更新失败: {error}");
                send_update_event(&event_tx, &ctx, UpdateEvent::Starting);
            }
        }
        Ok(velopack::UpdateCheck::RemoteIsEmpty) => {
            log::warn!("GitHub 更新源未提供可用版本");
            send_update_event(&event_tx, &ctx, UpdateEvent::Starting);
        }
        Ok(velopack::UpdateCheck::NoUpdateAvailable) => {
            log::info!("当前已是最新版本");
            send_update_event(&event_tx, &ctx, UpdateEvent::Starting);
        }
        Err(error) => {
            log::warn!("检查更新失败: {error}");
            send_update_event(&event_tx, &ctx, UpdateEvent::Starting);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn send_update_event(event_tx: &Sender<UpdateEvent>, ctx: &egui::Context, event: UpdateEvent) {
    let _ = event_tx.send(event);
    ctx.request_repaint();
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
