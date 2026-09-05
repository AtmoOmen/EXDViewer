//! Reporting paths the community list does not know.
//!
//! Parsed files name other files, so the viewer routinely holds a path ResLogger has never logged.
//! A path that hashes into the install's unnamed index entries is one the packages carry and the
//! list has no name for, which is the only pair worth sending on.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

use egui::{Color32, RichText};
use ironworks::sqpack::IndexHash;
use serde::Serialize;

use crate::{
    backend::Backend,
    settings::{REPORT_PATHS, REPORT_WINDOW_SHOWN},
    utils::{TrackedPromise, request},
};

/// How long a path waits for company before its batch goes out.
const DEBOUNCE: Duration = Duration::from_secs(5);

/// How long a rejected batch waits before it is offered again.
const RETRY: Duration = Duration::from_secs(30);

/// Paths per request, the same count ResLogger's own plugin sends.
const BATCH: usize = 250;

/// Candidates held at once; the oldest is dropped past this.
const QUEUE: usize = 512;

/// Queued paths the window lists before it starts counting instead.
const LISTED: usize = 20;

/// The first segment of every path the list carries.
const CATEGORIES: [&str; 12] = [
    "bg",
    "bgcommon",
    "chara",
    "common",
    "cut",
    "exd",
    "game_script",
    "music",
    "shader",
    "sound",
    "ui",
    "vfx",
];

/// A path in the form the packages hash it, alongside the case it actually appeared in.
///
/// The packages hash the lowercased path, so `hash` is what gets matched and sent; `display` keeps
/// the original case, since that is what the user typed or the game shipped.
struct Canonical {
    hash: String,
    display: String,
}

/// `path` in the form the packages hash it, or nothing when it cannot be a real name.
///
/// An unnamed file is drawn as its hash, so a synthesised name is eight hex digits and carries no
/// extension. Requiring one refuses both that and a directory the tree could only name by hash.
fn canonical(path: &str) -> Option<Canonical> {
    let display = path.trim().trim_matches('/');
    let hash = display.to_lowercase();
    if hash.len() > 256
        || !hash
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    {
        return None;
    }

    let mut segments = hash.split('/');
    if !CATEGORIES.contains(&segments.next()?) {
        return None;
    }
    let (stem, extension) = segments.next_back()?.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    segments
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        .then_some(Canonical {
            hash,
            display: display.to_string(),
        })
}

/// The index entries this install carries that the list has no name for, keyed by hash.
struct Unknown {
    split: Vec<u64>,
    whole: Vec<u32>,
}

impl Unknown {
    fn build(presence: &pathlist::Presence) -> Self {
        let mut split = Vec::new();
        let mut whole = Vec::new();
        for file in presence.unnamed() {
            if file.split {
                split.push(file.hash);
            } else {
                whole.push(file.hash as u32);
            }
        }
        split.sort_unstable();
        whole.sort_unstable();
        Self { split, whole }
    }

    fn contains(&self, path: &str) -> bool {
        let (split, whole) = IndexHash::of(path);
        matches!(split, Some(IndexHash::Split(hash)) if self.split.binary_search(&hash).is_ok())
            || matches!(whole, IndexHash::Whole(hash) if self.whole.binary_search(&hash).is_ok())
    }
}

/// A queued path, kept in both the form it is sent as and the form it is shown as.
#[derive(Clone)]
struct Queued {
    hash: String,
    display: String,
}

#[derive(Default)]
struct State {
    unknown: Option<Unknown>,
    queue: Vec<Queued>,
    seen: HashSet<String>,
    flush_at: Option<Instant>,
    upload: Option<TrackedPromise<Result<usize, String>>>,
}

pub struct Reporter {
    url: String,
    /// Mirrored from the setting, since a path is offered from a read with no context to hand.
    recording: Cell<bool>,
    state: RefCell<State>,
}

impl Reporter {
    pub fn new(api_url: &str) -> Self {
        Self {
            url: format!("{}/report/", api_url.trim_end_matches('/')),
            recording: Cell::new(true),
            state: RefCell::default(),
        }
    }

    /// Learn which of this install's files the list leaves unnamed.
    pub fn arm(&self, presence: &[u8]) {
        match pathlist::Presence::decode(presence) {
            Ok(presence) => self.state.borrow_mut().unknown = Some(Unknown::build(&presence)),
            Err(error) => log::warn!("report: {error}"),
        }
    }

    /// Offer a path the app has just proven the install carries.
    pub fn record(&self, path: &str) {
        if !self.recording.get() {
            return;
        }
        let Some(Canonical { hash, display }) = canonical(path) else {
            return;
        };
        // A read can land while the frame is already inside `poll`, and a candidate is offered
        // again the next time it is drawn, so dropping this one costs nothing.
        let Ok(mut state) = self.state.try_borrow_mut() else {
            return;
        };
        if state.seen.contains(&hash) {
            return;
        }
        if !state.unknown.as_ref().is_some_and(|u| u.contains(&hash)) {
            return;
        }
        if state.queue.len() == QUEUE {
            let dropped = state.queue.remove(0);
            state.seen.remove(&dropped.hash);
        }
        state.seen.insert(hash.clone());
        state.queue.push(Queued { hash, display });
        state.flush_at = Some(Instant::now() + DEBOUNCE);
    }

    fn poll(&self, ctx: &egui::Context) {
        let consent = REPORT_PATHS.get(ctx);
        self.recording.set(consent != Some(false));

        let mut state = self.state.borrow_mut();
        if consent == Some(false) {
            state.queue.clear();
            return;
        }
        if let Some(result) = state.upload.as_ref().and_then(|u| u.try_get().cloned()) {
            state.upload = None;
            match result {
                Ok(sent) => {
                    let sent = sent.min(state.queue.len());
                    state.queue.drain(..sent);
                    if !state.queue.is_empty() {
                        state.flush_at = Some(Instant::now());
                    }
                }
                Err(error) => {
                    log::warn!("report: {error}");
                    state.flush_at = Some(Instant::now() + RETRY);
                }
            }
        }

        if consent != Some(true) || state.upload.is_some() || state.queue.is_empty() {
            return;
        }
        let Some(at) = state.flush_at else {
            return;
        };
        let now = Instant::now();
        if now < at {
            ctx.request_repaint_after(at - now);
            return;
        }

        state.flush_at = None;
        let batch: Vec<String> = state
            .queue
            .iter()
            .take(BATCH)
            .map(|q| q.hash.clone())
            .collect();
        let count = batch.len();
        let url = self.url.clone();
        state.upload = Some(TrackedPromise::spawn_local(async move {
            send(&url, &batch)
                .await
                .map(|()| count)
                .map_err(|error| error.to_string())
        }));
    }
}

#[derive(Serialize)]
struct Submission<'a> {
    paths: &'a [String],
}

async fn send(url: &str, paths: &[String]) -> anyhow::Result<()> {
    let body = serde_json::to_vec(&Submission { paths })?;
    let response = request(
        "POST",
        url,
        &[("Content-Type", "application/json")],
        Some(body),
    )
    .await?;
    anyhow::ensure!(
        response.ok,
        "服务器对 {} 个路径返回了 {}",
        response.status,
        paths.len()
    );
    Ok(())
}

/// A small menu-bar badge: something was found, and clicking it opens the window rather than
/// asking right there.
pub fn indicator(ui: &mut egui::Ui, backend: Option<&Backend>) {
    let Some(reporter) = backend.map(Backend::reporter) else {
        return;
    };
    reporter.poll(ui.ctx());

    if REPORT_PATHS.get(ui.ctx()).is_some() {
        return;
    }
    let pending = reporter.state.borrow().queue.len();
    if pending == 0 {
        return;
    }

    if ui
        .button(format!("{pending} 个新路径名称",))
        .on_hover_text(
            "当前安装中社区路径列表不知道的文件名。点击查看。",
        )
        .clicked()
    {
        REPORT_WINDOW_SHOWN.set(ui.ctx(), true);
    }
}

/// The path-report window: the one-time ask, and where "Show names" lives once it has been
/// answered.
pub fn draw_window(ctx: &egui::Context, backend: Option<&Backend>) {
    let Some(reporter) = backend.map(Backend::reporter) else {
        return;
    };
    let mut shown = REPORT_WINDOW_SHOWN.get(ctx);
    let was_shown = shown;
    egui::Window::new("社区路径报告")
        .open(&mut shown)
        .show(ctx, |ui| {
            let queued = reporter.state.borrow().queue.clone();
            if queued.is_empty() {
                ui.label("尚未发现新的文件名。");
                return;
            }

            ui.label(format!(
                "社区路径列表不知道 {} 个文件名。",
                queued.len(),
            ));

            match REPORT_PATHS.get(ctx) {
                None => {
                    ui.horizontal(|ui| {
                        if ui
                            .button("上报")
                            .on_hover_text(
                                "通过 XIViewer API 将这些文件名发送给 ResLogger2，每个人的浏览器都能 \
                                 显示它们。除此之外不会发送与你的会话相关的任何信息。",
                            )
                            .clicked()
                        {
                            REPORT_PATHS.set(ctx, Some(true));
                        }
                        if ui
                            .button("不用了")
                            .on_hover_text("已记住，不会再询问。")
                            .clicked()
                        {
                            REPORT_PATHS.set(ctx, Some(false));
                        }
                    });
                }
                Some(true) => {
                    ui.label(RichText::new("已开启路径上报。").weak());
                }
                Some(false) => {
                    ui.label(RichText::new("已关闭路径上报。").weak());
                }
            }

            ui.collapsing("显示名称", |ui| {
                for path in queued.iter().take(LISTED) {
                    ui.label(RichText::new(&path.display).monospace().color(Color32::GRAY));
                }
                if let Some(rest) = queued.len().checked_sub(LISTED).filter(|rest| *rest > 0) {
                    ui.label(RichText::new(format!("另有 {rest} 个")).italics());
                }
            });
        });
    if shown != was_shown {
        REPORT_WINDOW_SHOWN.set(ctx, shown);
    }
}

/// The settings entries: whether reporting is on, and whether the window above is open.
pub fn menu_item(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let mut enabled = REPORT_PATHS.get(&ctx) == Some(true);
    if ui
        .checkbox(&mut enabled, "向 ResLogger2 上报新路径")
        .on_hover_text(
            "当前安装中社区路径列表不知道的文件名会通过 XIViewer API 上报。仅路径，不含任何 \
             可识别身份的信息。",
        )
        .changed()
    {
        REPORT_PATHS.set(&ctx, Some(enabled));
    }

    let mut window_shown = REPORT_WINDOW_SHOWN.get(&ctx);
    if ui
        .checkbox(&mut window_shown, "显示路径报告窗口")
        .changed()
    {
        REPORT_WINDOW_SHOWN.set(&ctx, window_shown);
    }
}

/// Wraps a provider so every path it proves the install carries is offered to the reporter.
pub struct Recording {
    inner: Rc<dyn crate::data::FileProvider>,
    reporter: Rc<Reporter>,
}

impl Recording {
    pub fn new(inner: Rc<dyn crate::data::FileProvider>, reporter: Rc<Reporter>) -> Self {
        Self { inner, reporter }
    }
}

#[async_trait::async_trait(?Send)]
impl crate::data::FileProvider for Recording {
    async fn read_stream(&self, path: &str) -> anyhow::Result<(Option<String>, Vec<u8>)> {
        let read = self.inner.read_stream(path).await;
        if read.is_ok() {
            self.reporter.record(path);
        }
        read
    }

    async fn read_stream_by_hash(
        &self,
        repository: u8,
        category: u8,
        hash: u64,
        split: bool,
    ) -> anyhow::Result<(Option<String>, Vec<u8>)> {
        self.inner
            .read_stream_by_hash(repository, category, hash, split)
            .await
    }

    async fn path_index(&self, api_base: &str) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        let index = self.inner.path_index(api_base).await?;
        self.reporter.arm(&index.1);
        Ok(index)
    }

    // Forwarded rather than left to the default, which would decode in this thread and lose the
    // worker provider's own decode.
    async fn read_texture(
        &self,
        path: &str,
        max_dim: Option<u16>,
    ) -> anyhow::Result<crate::data::DecodedTexture> {
        let decoded = self.inner.read_texture(path, max_dim).await;
        if decoded.is_ok() {
            self.reporter.record(path);
        }
        decoded
    }

    async fn read_model(&self, path: &str, lod: u8) -> anyhow::Result<(Vec<u8>, u8)> {
        let read = self.inner.read_model(path, lod).await;
        if read.is_ok() {
            self.reporter.record(path);
        }
        read
    }

    async fn read_package(&self, path: &str) -> anyhow::Result<(Vec<u8>, bool)> {
        let read = self.inner.read_package(path).await;
        if read.is_ok() {
            self.reporter.record(path);
        }
        read
    }

    async fn read_span(&self, path: &str, span: std::ops::Range<u32>) -> anyhow::Result<Vec<u8>> {
        self.inner.read_span(path, span).await
    }

    async fn get_icon(
        &self,
        path: &str,
    ) -> anyhow::Result<either::Either<url::Url, image::RgbaImage>> {
        self.inner.get_icon(path).await
    }

    async fn exists_many(&self, paths: &[String]) -> anyhow::Result<Vec<bool>> {
        let found = self.inner.exists_many(paths).await?;
        for (path, _) in paths.iter().zip(&found).filter(|(_, found)| **found) {
            self.reporter.record(path);
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathlist::{Presence, Unnamed, encode_presence};

    fn unknown(paths: &[&str]) -> Unknown {
        let files: Vec<Unnamed> = paths
            .iter()
            .map(|path| {
                let (split, _) = IndexHash::of(&path.to_lowercase());
                let IndexHash::Split(hash) = split.unwrap() else {
                    unreachable!()
                };
                Unnamed {
                    repository: 0,
                    category: 6,
                    hash,
                    split: true,
                }
            })
            .collect();
        Unknown::build(&Presence::decode(&encode_presence(&[], &files, 1)).unwrap())
    }

    /// 7.3% of listed names carry a capital and the packages hash the lowercased form, so a test
    /// spelled only in lowercase passes whether or not the canonical form lowercases.
    #[test]
    fn a_mixed_case_name_hashes_lowercase_but_displays_as_typed() {
        let unknown = unknown(&["sound/voice/Vo_Emote/vo_emote_battlecry_01.scd"]);
        let path = canonical("sound/voice/Vo_Emote/Vo_Emote_BattleCry_01.scd").unwrap();
        assert_eq!(path.hash, "sound/voice/vo_emote/vo_emote_battlecry_01.scd");
        assert_eq!(path.display, "sound/voice/Vo_Emote/Vo_Emote_BattleCry_01.scd");
        assert!(unknown.contains(&path.hash));
        assert!(!unknown.contains(&canonical("ui/uld/mkdrelicgrowth3.uld").unwrap().hash));
    }

    #[test]
    fn a_synthesised_name_is_not_a_path() {
        assert!(canonical("ui/uld/1f01a2d3").is_none());
        assert!(canonical("music/ex4/12345678/000000ff").is_none());
        assert_eq!(
            canonical("music/ex4/12345678/bgm_ex4_01.scd").map(|c| c.hash),
            Some("music/ex4/12345678/bgm_ex4_01.scd".to_string())
        );
    }

    #[test]
    fn only_a_real_game_path_survives() {
        for bad in [
            "ui/uld/foo bar.uld",
            "ui/uld/foo#bar.uld",
            "notacategory/foo.uld",
            "ui/uld/noextension",
            "ui/uld/.uld",
            "ui/uld/foo.",
            "ui/..",
            "ui/../foo.uld",
            "ui//foo.uld",
            "ui",
        ] {
            assert!(canonical(bad).is_none(), "{bad}");
        }
        let path = canonical("  /UI/Uld/Foo-Bar_1.uld/ ").unwrap();
        assert_eq!(path.hash, "ui/uld/foo-bar_1.uld");
        assert_eq!(path.display, "UI/Uld/Foo-Bar_1.uld");
    }
}
