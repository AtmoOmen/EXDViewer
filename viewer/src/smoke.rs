//! Drives the real native app offscreen and screenshots it, for `main.rs`'s `--smoke` mode. Plays
//! the same role `smoke/smoke.ts` plays over CDP against the wasm build: open an asset, wait for it
//! to decode, click the same control-row positions, and shoot the result. Failure is a panic
//! (native eframe does not catch one; the process exits non-zero and the harness sees it), an
//! ERROR-level log, or a step that times out without decoding.
//!
//! Navigation is seeded through the same `egui::Context` keys [`crate::router::history::memory`]
//! reads, since `App::navigate` is private to `app.rs` and this module cannot add a public door to
//! it without touching a file another agent owns.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use egui::{Event, Id, Modifiers, PointerButton, Pos2, ViewportCommand, pos2};
use log::{Level, Log, Metadata, Record};

use crate::{
    App,
    router::path::Path,
    settings::{BACKEND_CONFIG, BackendConfig, InstallLocation, SchemaLocation},
};

/// One asset to open, and where its post-load control row sits so the right spot gets clicked
/// once it is on screen. Coordinates are relative to `Config::width`/`height` at 1x scale, the
/// same viewport the browser gate uses.
pub enum Step {
    /// `/assets/<mdl>`, clicking "Game shaders" once it has decoded.
    Model(String),
    /// `/assets/<lgb-or-lvb>`, clicking the "Scene" tab once it has decoded.
    Scene(String),
    /// `/assets/<cutb>`, clicking the "Play" tab once it has decoded.
    Cut(String),
}

impl Step {
    fn path(&self) -> &str {
        match self {
            Step::Model(p) | Step::Scene(p) | Step::Cut(p) => p,
        }
    }

    fn route(&self) -> String {
        format!("/assets/{}", self.path())
    }

    fn click_at(&self) -> Pos2 {
        match self {
            Step::Model(_) => pos2(GAME_SHADERS_X, ROW_Y),
            Step::Scene(_) => pos2(SCENE_TAB_X, ROW_Y),
            Step::Cut(_) => pos2(PLAY_TAB_X, ROW_Y),
        }
    }
}

// Calibrated against a 1600x1000 viewport, the same layout the browser gate's `smoke.ts` clicks.
const ROW_Y: f32 = 116.0;
const GAME_SHADERS_X: f32 = 267.0;
const SCENE_TAB_X: f32 = 287.0;
const PLAY_TAB_X: f32 = 406.0;

pub struct Config {
    pub sqpack_path: String,
    pub schema_path: Option<String>,
    pub steps: Vec<Step>,
    pub out_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub step_timeout: Duration,
}

/// A `log::Log` wrapper that counts ERROR-level records, mirroring the browser gate's `ERROR:` /
/// `console.error` watch. `main.rs` installs one of these as the process logger in `--smoke` mode.
pub struct CountingLogger<L: Log + 'static> {
    inner: L,
    counters: Counters,
}

impl<L: Log + 'static> CountingLogger<L> {
    pub fn new(inner: L) -> (Self, Counters) {
        let counters = Counters::default();
        (
            Self {
                inner,
                counters: counters.clone(),
            },
            counters,
        )
    }

    pub fn init(self) {
        log::set_boxed_logger(Box::new(self)).expect("Failed to set logger");
    }
}

impl<L: Log + 'static> Log for CountingLogger<L> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        let text = record.args().to_string();
        if record.level() == Level::Error {
            self.counters.errors.fetch_add(1, Ordering::SeqCst);
            self.counters.messages.lock().unwrap().push(text.clone());
        }
        // The line `assets/preview: <viewer> in <time>` fires once a viewer has actually decoded
        // its bytes, which is what says a route change turned into a rendered file rather than
        // just a URL change.
        if text.starts_with("assets/preview: ") {
            self.counters.decoded.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

#[derive(Clone, Default)]
pub struct Counters {
    errors: Arc<AtomicUsize>,
    decoded: Arc<AtomicUsize>,
    messages: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Counters {
    fn decoded(&self) -> usize {
        self.decoded.load(Ordering::SeqCst)
    }

    pub fn error_count(&self) -> usize {
        self.errors.load(Ordering::SeqCst)
    }

    pub fn error_messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }
}

/// Writes a path straight into the keys [`crate::router::history::memory::MemoryHistory`] reads,
/// since the router itself is seeded lazily on the app's first draw and there is no earlier public
/// hook to hand it a starting route.
fn seed_route(ctx: &egui::Context, path: Path) {
    ctx.data_mut(|d| {
        d.insert_persisted(Id::new("memory_history"), vec![path]);
        d.insert_persisted(Id::new("memory_history_position"), 0usize);
    });
}

enum Phase {
    Opening { at: Instant, opened: usize },
    /// Waiting out the initial settle before the pre-click reference shot.
    Settling { at: Instant },
    /// The reference shot has been requested; `before` lands via `Event::Screenshot`.
    Referencing { at: Instant },
    Clicked { at: Instant },
    Shooting { at: Instant, requested: bool },
}

pub struct SmokeApp {
    app: App,
    config: Config,
    counters: Counters,
    step: usize,
    phase: Phase,
    click: Option<Pos2>,
    /// The frame taken just before the click, kept only to prove the click changed something.
    reference: Option<Arc<egui::ColorImage>>,
    failure: Option<String>,
    outcomes: Vec<StepOutcome>,
}

struct StepOutcome {
    path: String,
    screenshot: PathBuf,
}

impl SmokeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config, counters: Counters) -> Self {
        let backend_config = BackendConfig {
            api_url: crate::DEFAULT_API_URL.to_string(),
            location: InstallLocation::Sqpack(config.sqpack_path.clone()),
            schema: config
                .schema_path
                .clone()
                .map(SchemaLocation::Local)
                .unwrap_or_else(|| SchemaLocation::Web(crate::DEFAULT_SCHEMA_URL.to_string())),
        };
        BACKEND_CONFIG.set(&cc.egui_ctx, Some(backend_config));

        let first = config.steps.first().map_or("/".to_string(), Step::route);
        // The setup route only auto-submits when its URL carries a `redirect`, the same query
        // param a real deep link bounces through `ensure_backend` with.
        let start = Path::with_params("/", [("redirect", first.as_str())]);
        seed_route(&cc.egui_ctx, start);

        Self {
            app: App::new(cc),
            config,
            counters,
            step: 0,
            phase: Phase::Opening {
                at: Instant::now(),
                opened: 0,
            },
            click: None,
            reference: None,
            failure: None,
            outcomes: Vec::new(),
        }
    }

    fn fail(&mut self, why: impl Into<String>) {
        if self.failure.is_none() {
            let why = why.into();
            log::error!("smoke: {why}");
            self.failure = Some(why);
        }
    }

    fn finish(&self) {
        let report = Report {
            steps: self
                .outcomes
                .iter()
                .map(|o| ReportStep {
                    path: o.path.clone(),
                    screenshot: o.screenshot.display().to_string(),
                })
                .collect(),
            errors: self.counters.error_messages(),
            failure: self.failure.clone(),
        };
        let text = serde_json::to_string_pretty(&report).unwrap_or_default();
        let _ = std::fs::write(self.config.out_dir.join("report.json"), text);
        let ok = self.failure.is_none();
        log::info!("smoke: {}", if ok { "PASS" } else { "FAIL" });
        std::process::exit(if ok { 0 } else { 1 });
    }

    /// The post-click shot: checked against the reference taken before the click (a click that
    /// missed its target leaves the frame unchanged) and against a blank frame (a click that hit
    /// nothing, or a pass that silently drew into the wrong buffer), then saved and advanced.
    fn land_shot(&mut self, ctx: &egui::Context, image: Arc<egui::ColorImage>) {
        let Some(step) = self.config.steps.get(self.step) else {
            return;
        };
        let path = step.path().to_string();

        if self.reference.as_ref().is_some_and(|before| same(before, &image)) {
            self.fail(format!(
                "{path}: the frame after the click is identical to the one before it; the click \
                 never landed on \"Game shaders\" or \"Scene\""
            ));
            return;
        }
        if blank(&image) {
            self.fail(format!("{path}: the shot after the click is a single flat color"));
            return;
        }

        let name = crate::utils::file_name(&path);
        let out = self.config.out_dir.join(format!("{name}.png"));
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
        if let Err(e) = image::save_buffer(
            &out,
            &rgba,
            image.width() as u32,
            image.height() as u32,
            image::ColorType::Rgba8,
        ) {
            self.fail(format!("could not save screenshot for {path}: {e}"));
            return;
        }
        self.outcomes.push(StepOutcome {
            path,
            screenshot: out,
        });

        self.reference = None;
        self.step += 1;
        match self.config.steps.get(self.step) {
            Some(next) => {
                ctx.data_mut(|d| {
                    let history: &mut Vec<Path> = d
                        .get_persisted_mut_or_insert_with(Id::new("memory_history"), || {
                            vec![Path::parse("/")]
                        });
                    history.push(Path::parse(&next.route()));
                    let position: &mut usize =
                        d.get_persisted_mut_or_insert_with(Id::new("memory_history_position"), || 0);
                    *position += 1;
                });
                self.phase = Phase::Opening {
                    at: Instant::now(),
                    opened: self.counters.decoded(),
                };
            }
            None => self.finish(),
        }
    }
}

/// Whether two shots of the same size are close enough to call unchanged: exact equality is too
/// strict once anything animates (water, foliage, the effect clock), so this tolerates a handful
/// of differing pixels rather than none.
fn same(a: &egui::ColorImage, b: &egui::ColorImage) -> bool {
    a.size == b.size
        && a.pixels
            .iter()
            .zip(&b.pixels)
            .filter(|(x, y)| x != y)
            .count()
            < a.pixels.len() / 1000 + 1
}

/// Whether a shot is close enough to one flat color to be worth failing on: a viewport that never
/// drew anything, or a pass that cleared its target and stopped.
fn blank(image: &egui::ColorImage) -> bool {
    let Some(first) = image.pixels.first() else {
        return true;
    };
    image
        .pixels
        .iter()
        .filter(|p| *p != first)
        .count()
        < image.pixels.len() / 1000 + 1
}

impl eframe::App for SmokeApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut eframe::egui::RawInput) {
        let Some(pos) = self.click.take() else {
            return;
        };
        raw_input.events.push(Event::PointerMoved(pos));
        raw_input.events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        raw_input.events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        let ctx = ui.ctx().clone();

        if self.failure.is_none() && self.counters.error_count() > 0 {
            // The counting logger already printed it; failing here just stops the run instead of
            // idling out to the step timeout on top of the error that already doomed it.
            self.failure = Some(format!("{} ERROR-level log(s)", self.counters.error_count()));
        }

        for event in ctx.input(|i| i.events.clone()) {
            let Event::Screenshot { image, .. } = event else {
                continue;
            };
            match &self.phase {
                Phase::Referencing { .. } => {
                    self.reference = Some(image);
                    self.click = Some(self.config.steps[self.step].click_at());
                    self.phase = Phase::Clicked { at: Instant::now() };
                }
                Phase::Shooting { .. } => self.land_shot(&ctx, image),
                _ => {}
            }
        }

        if self.failure.is_some() {
            self.finish();
            return;
        }

        let Some(step) = self.config.steps.get(self.step) else {
            self.finish();
            return;
        };

        match &self.phase {
            Phase::Opening { at, opened } => {
                if self.counters.decoded() > *opened {
                    self.phase = Phase::Settling { at: Instant::now() };
                } else if at.elapsed() > self.config.step_timeout {
                    self.fail(format!("{} never decoded", step.path()));
                }
            }
            Phase::Settling { at } => {
                if at.elapsed() > Duration::from_millis(800) {
                    ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
                    self.phase = Phase::Referencing { at: Instant::now() };
                }
            }
            Phase::Referencing { at } => {
                if at.elapsed() > self.config.step_timeout {
                    self.fail(format!("{} never produced a reference shot", step.path()));
                }
            }
            Phase::Clicked { at } => {
                // A scene's instances stream in over several seconds; a model's shaded frame
                // settles inside one. Matches `smoke.ts`'s `SETTLE` for the same reason.
                let settle = match step {
                    Step::Model(_) => Duration::from_secs(2),
                    Step::Scene(_) => Duration::from_secs(8),
                    // A cutscene streams its level and builds a character per participant on top
                    // of it, each of which is a dozen models of its own.
                    Step::Cut(_) => Duration::from_secs(75),
                };
                if at.elapsed() > settle {
                    self.phase = Phase::Shooting {
                        at: Instant::now(),
                        requested: false,
                    };
                }
            }
            Phase::Shooting { at, requested } => {
                if !requested {
                    ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
                    self.phase = Phase::Shooting {
                        at: *at,
                        requested: true,
                    };
                } else if at.elapsed() > self.config.step_timeout {
                    self.fail(format!("{} never produced a screenshot", step.path()));
                }
            }
        }

        ctx.request_repaint();
    }
}

#[derive(serde::Serialize)]
struct ReportStep {
    path: String,
    screenshot: String,
}

#[derive(serde::Serialize)]
struct Report {
    steps: Vec<ReportStep>,
    errors: Vec<String>,
    failure: Option<String>,
}
