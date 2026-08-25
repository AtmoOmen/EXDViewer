//! The zone tab: `TerritoryType` rows mapped to their `.lvb`, hosting the existing layer/scene
//! viewer behind a picker of its own rather than reading it as a raw asset.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use anyhow::Result;
use egui::{
    Align, Button, CentralPanel, Color32, Layout, RichText, ScrollArea, TextEdit, Vec2, Widget,
    containers::panel::Panel,
};
use ironworks::excel::Language;

use crate::assets::deps::Deps;
use crate::assets::viewers::Preview;
use crate::assets::viewers::layer;
use crate::backend::Backend;
use crate::data::FileProvider;
use crate::excel::base::CachedProvider;
use crate::excel::provider::{ExcelProvider, ExcelSheet};
use crate::goto::{ListNav, Palette, SUGGESTIONS};
use crate::settings::LANGUAGE;
use crate::utils::{
    CollapsibleSidePanel, FuzzyMatcher, PromiseKind, Side, TrackedPromise, empty_view,
};

const FILTER_ID: &str = "zone_filter";
const LIST_WIDTH: f32 = 320.0;
const DETAILS_WIDTH: f32 = 400.0;

/// `TerritoryType`'s Name, Bg and PlaceName columns, as byte offsets. Schema field order is offset
/// order, not raw exh column order; verified against the schema and a live sweep.
const NAME: u32 = 0;
const BG: u32 = 4;
const PLACE_NAME: u32 = 32;

/// One `TerritoryType` row that names a level.
struct Zone {
    place_name: u32,
    /// The internal short code, e.g. `s1t1`. Falls in for a row `PlaceName` leaves unnamed.
    name: String,
    path: String,
}

enum Index {
    Idle,
    Loading(TrackedPromise<Result<Vec<Zone>>>),
    Loaded(Vec<Zone>),
    Failed(String),
}

enum Avail {
    Idle,
    Loading(TrackedPromise<Result<HashSet<String>>>),
    Ready(HashSet<String>),
    Failed,
}

enum NamesLoad {
    Idle,
    Loading(TrackedPromise<Result<HashMap<u32, String>>>),
    Done,
}

enum Open {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Box<layer::Rendered>),
    Failed(String),
}

struct Opened {
    path: String,
    state: Open,
}

struct Row {
    path: String,
    name: String,
    available: bool,
}

pub enum Action {
    /// A zone was picked from the list or the palette; reflect it in the URL.
    Select(String),
    /// The placed scene or its tree followed a link to a file the zone itself is not, which the
    /// Assets tab knows how to open.
    Navigate(String),
}

pub struct ZoneBrowser {
    index: Index,
    avail: Avail,
    /// `PlaceName`'s own text, by row id. Kept across a load failure so a stale but real name beats
    /// a blank one; only cleared on [`ZoneBrowser::reset`].
    names: HashMap<u32, String>,
    names_load: NamesLoad,
    names_lang: Option<Language>,
    rows: Vec<Row>,
    rows_stale: bool,
    search: String,
    show_unavailable: bool,
    matcher: FuzzyMatcher,
    palette: Option<Palette>,
    nav: ListNav,
    pending: Option<String>,
    opened: Option<Opened>,
    deps: Deps,
}

impl Default for ZoneBrowser {
    fn default() -> Self {
        Self {
            index: Index::Idle,
            avail: Avail::Idle,
            names: HashMap::new(),
            names_load: NamesLoad::Idle,
            names_lang: None,
            rows: Vec::new(),
            rows_stale: true,
            search: String::new(),
            show_unavailable: false,
            matcher: FuzzyMatcher::new(),
            palette: None,
            nav: ListNav::default(),
            pending: None,
            opened: None,
            deps: Deps::default(),
        }
    }
}

impl ZoneBrowser {
    /// The zone open or about to be, so entering the bare route restores the same URL either way.
    pub fn selected(&self) -> Option<String> {
        self.opened
            .as_ref()
            .map(|o| o.path.clone())
            .or_else(|| self.pending.clone())
    }

    pub fn name_of(&self, path: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|row| row.path == path)
            .map(|row| row.name.as_str())
    }

    pub fn request(&mut self, path: String) {
        self.pending = Some(path);
    }

    /// Drop everything the previous install answered, and re-arm whatever was open so it is read
    /// again from the new one.
    pub fn reset(&mut self) {
        self.index = Index::Idle;
        self.avail = Avail::Idle;
        self.names.clear();
        self.names_load = NamesLoad::Idle;
        self.names_lang = None;
        self.rows_stale = true;
        self.pending = self.opened.take().map(|o| o.path).or(self.pending.take());
    }

    pub fn open_palette(&mut self) {
        self.palette = Some(Palette::new("Find Zone…", "Filter", self.search.clone()));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, backend: &Backend) -> Option<Action> {
        self.poll(backend, LANGUAGE.get(ui.ctx()));
        if self.rows_stale {
            self.rebuild_rows();
        }

        let picked = self.draw_palette(ui.ctx());
        let listed = matches!(self.index, Index::Loaded(_))
            && !CollapsibleSidePanel::is_collapsed(ui.ctx(), "zone_list");
        self.nav
            .claim(ui.ctx(), listed, Some(egui::Id::new(FILTER_ID)));

        let clicked = self.side_panel(ui);
        let followed = self.main_panel(ui, backend);
        picked
            .or(clicked)
            .map(Action::Select)
            .or_else(|| followed.map(Action::Navigate))
    }

    fn draw_palette(&mut self, ctx: &egui::Context) -> Option<String> {
        let palette = self.palette.take()?;
        match palette.draw(ctx, |query| {
            self.search = query.to_owned();
            self.matches()
                .into_iter()
                .take(SUGGESTIONS)
                .map(|row| (row.path.clone(), row.name.clone()))
                .collect()
        }) {
            Ok(picked) => picked,
            Err(palette) => {
                self.palette = Some(palette);
                None
            }
        }
    }

    fn poll(&mut self, backend: &Backend, lang: Language) {
        if matches!(self.index, Index::Idle) {
            let excel = backend.excel().clone();
            self.index = Index::Loading(TrackedPromise::spawn_local(async move {
                load_index(excel).await
            }));
        }
        if matches!(&self.index, Index::Loading(p) if p.try_get().is_some()) {
            let Index::Loading(promise) = std::mem::replace(&mut self.index, Index::Idle) else {
                unreachable!()
            };
            self.index = match promise.block_and_take() {
                Ok(zones) => Index::Loaded(zones),
                Err(error) => Index::Failed(error.to_string()),
            };
            self.rows_stale = true;
        }

        if self.names_lang != Some(lang) && !matches!(self.names_load, NamesLoad::Loading(_)) {
            self.names_lang = Some(lang);
            let excel = backend.excel().clone();
            self.names_load = NamesLoad::Loading(TrackedPromise::spawn_local(async move {
                load_names(excel, lang).await
            }));
        }
        if matches!(&self.names_load, NamesLoad::Loading(p) if p.try_get().is_some()) {
            let NamesLoad::Loading(promise) =
                std::mem::replace(&mut self.names_load, NamesLoad::Idle)
            else {
                unreachable!()
            };
            match promise.block_and_take() {
                Ok(names) => {
                    self.names = names;
                    self.rows_stale = true;
                }
                Err(error) => {
                    log::warn!("zones: PlaceName unavailable, using internal names: {error}")
                }
            }
            self.names_load = NamesLoad::Done;
        }

        if matches!(self.avail, Avail::Idle)
            && let Index::Loaded(zones) = &self.index
        {
            let files = backend.files().clone();
            let paths: Vec<String> = zones.iter().map(|zone| zone.path.clone()).collect();
            self.avail = Avail::Loading(TrackedPromise::spawn_local(async move {
                check_availability(files, paths).await
            }));
        }
        if matches!(&self.avail, Avail::Loading(p) if p.try_get().is_some()) {
            let Avail::Loading(promise) = std::mem::replace(&mut self.avail, Avail::Idle) else {
                unreachable!()
            };
            self.avail = match promise.block_and_take() {
                Ok(available) => Avail::Ready(available),
                Err(_) => Avail::Failed,
            };
            self.rows_stale = true;
        }

        self.poll_open();

        if let Some(path) = self.pending.take()
            && self.opened.as_ref().map(|o| o.path.as_str()) != Some(path.as_str())
        {
            self.open(backend, path);
        }
    }

    fn open(&mut self, backend: &Backend, path: String) {
        let files = backend.files().clone();
        let fetch_path = path.clone();
        let promise = TrackedPromise::spawn_local(async move { files.read(&fetch_path).await });
        self.opened = Some(Opened {
            path,
            state: Open::Fetching(promise),
        });
    }

    fn poll_open(&mut self) {
        let Some(opened) = &mut self.opened else {
            return;
        };
        let Open::Fetching(promise) = &opened.state else {
            return;
        };
        let Some(result) = promise.try_get() else {
            return;
        };
        opened.state = match result {
            Ok(bytes) => match layer::lvb::decode(&opened.path, bytes) {
                Ok(Preview::Layers(rendered)) => {
                    // The tab exists to show the placed scene, not the raw tree.
                    rendered.show_scene();
                    Open::Ready(rendered)
                }
                Ok(_) => unreachable!("lvb decode always yields a layer preview"),
                Err(error) => Open::Failed(error.to_string()),
            },
            Err(error) => Open::Failed(error.to_string()),
        };
    }

    fn friendly(&self, zone: &Zone) -> String {
        if zone.place_name != 0
            && let Some(name) = self.names.get(&zone.place_name)
        {
            return name.clone();
        }
        if !zone.name.is_empty() {
            return zone.name.clone();
        }
        zone.path.clone()
    }

    fn rebuild_rows(&mut self) {
        let Index::Loaded(zones) = &self.index else {
            return;
        };
        let rows = zones
            .iter()
            .map(|zone| {
                let available = match &self.avail {
                    Avail::Ready(set) => set.contains(&zone.path),
                    _ => true,
                };
                Row {
                    path: zone.path.clone(),
                    name: self.friendly(zone),
                    available,
                }
            })
            .collect();
        self.rows = rows;
        self.rows_stale = false;
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut clicked = None;
        let mut nav = std::mem::take(&mut self.nav);
        CollapsibleSidePanel::new("zone_list", Side::Left)
            .max_width(LIST_WIDTH)
            .show(ui, |ui, is_open| {
                if !is_open {
                    return;
                }
                Panel::top("zone_list_header").show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            CollapsibleSidePanel::draw_arrow(ui, "zone_list", Side::Left);
                            ui.vertical_centered_justified(|ui| ui.heading("Zones"));
                        });
                    });
                    ui.add_space(4.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui
                            .add_enabled(!self.search.is_empty(), Button::new("↩"))
                            .on_hover_text("Clear")
                            .clicked()
                        {
                            self.search.clear();
                        }
                        let unavailable = self.rows.iter().filter(|row| !row.available).count();
                        if unavailable > 0 {
                            ui.toggle_value(&mut self.show_unavailable, "🚫")
                                .on_hover_text(format!("Show {unavailable} unavailable"));
                        }
                        ui.add_sized(
                            Vec2::new(ui.available_width(), 0.0),
                            TextEdit::singleline(&mut self.search)
                                .id(egui::Id::new(FILTER_ID))
                                .hint_text("Filter"),
                        );
                    });
                    ui.add_space(4.0);
                });

                CentralPanel::default().show(ui, |ui| match &self.index {
                    Index::Idle | Index::Loading(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading zone list…");
                        });
                    }
                    Index::Failed(error) => {
                        ui.colored_label(
                            Color32::RED,
                            format!("Failed to load zone list: {error}"),
                        );
                    }
                    Index::Loaded(_) => clicked = self.draw_rows(ui, &mut nav),
                });
            });
        self.nav = nav;
        clicked
    }

    /// The zones the filter leaves, best match first.
    fn matches(&self) -> Vec<&Row> {
        let query = (!self.search.is_empty()).then_some(self.search.as_str());
        self.matcher.match_list_indirect(
            query,
            self.rows
                .iter()
                .filter(|row| self.show_unavailable || row.available),
            |row| row.name.as_str(),
        )
    }

    fn draw_rows(&self, ui: &mut egui::Ui, nav: &mut ListNav) -> Option<String> {
        let selected = self.opened.as_ref().map(|o| o.path.as_str());
        let filtered = self.matches();

        let mut clicked = nav
            .apply(filtered.len())
            .filter(|at| filtered[*at].available)
            .map(|at| filtered[at].path.clone());
        let row_height = ui.text_style_height(&egui::TextStyle::Button);
        let mut area = ScrollArea::vertical().auto_shrink(false);
        if let Some(offset) = nav.scroll(ui, row_height, filtered.len()) {
            area = area.vertical_scroll_offset(offset);
        }
        let output = area.show_rows(ui, row_height, filtered.len(), |ui, range| {
            ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                for (at, row) in filtered[range.clone()].iter().enumerate() {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    let response = ui
                        .add_enabled_ui(row.available, |ui| {
                            Button::selectable(
                                selected == Some(row.path.as_str()),
                                row.name.as_str(),
                            )
                            .ui(ui)
                        })
                        .inner
                        .on_hover_ui(|ui| self.row_hover(ui, row));
                    nav.mark(ui, range.start + at, response.rect);
                    if response.clicked() {
                        clicked = Some(row.path.clone());
                    }
                }
            });
        });
        nav.seen(&output);
        clicked
    }

    fn row_hover(&self, ui: &mut egui::Ui, row: &Row) {
        ui.strong(&row.name);
        ui.separator();
        ui.label(RichText::new(&row.path).weak());
        if !row.available {
            ui.colored_label(
                Color32::from_rgb(0xE0, 0x8C, 0x3C),
                "Not available on this data source",
            );
        }
    }

    fn main_panel(&mut self, ui: &mut egui::Ui, backend: &Backend) -> Option<String> {
        let mut follow = None;

        if let Some(opened) = &self.opened
            && let Open::Ready(rendered) = &opened.state
        {
            CollapsibleSidePanel::new("zone_info", Side::Right)
                .max_width(DETAILS_WIDTH)
                .show(ui, |ui, is_open| {
                    if !is_open {
                        return;
                    }
                    Panel::top("zone_info_header").show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                CollapsibleSidePanel::draw_arrow(ui, "zone_info", Side::Right);
                                ui.vertical_centered_justified(|ui| ui.heading("Details"));
                            });
                        });
                        ui.add_space(4.0);
                    });
                    CentralPanel::default().show(ui, |ui| {
                        rendered.details_ui(ui, &mut follow, &mut self.deps, backend);
                    });
                });
        }

        CentralPanel::default().show(ui, |ui| {
            if CollapsibleSidePanel::is_collapsed(ui.ctx(), "zone_list") {
                Panel::top("zone_list_reexpand").show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        CollapsibleSidePanel::draw_arrow(ui, "zone_list", Side::Left);
                    });
                    ui.add_space(4.0);
                });
            }

            let Some(opened) = &self.opened else {
                empty_view(ui, "🗐", "Select a zone to view");
                return;
            };

            Panel::top("zone_header").show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if CollapsibleSidePanel::is_collapsed(ui.ctx(), "zone_info") {
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            CollapsibleSidePanel::draw_arrow(ui, "zone_info", Side::Right);
                        });
                    }
                    // No sheet row names this path: shown as-is rather than hidden.
                    let title = self
                        .name_of(&opened.path)
                        .map(str::to_owned)
                        .unwrap_or_else(|| opened.path.clone());
                    ui.vertical_centered_justified(|ui| {
                        ui.heading(title).on_hover_text(&opened.path)
                    });
                });
                ui.add_space(4.0);
            });

            match &opened.state {
                Open::Fetching(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading zone…");
                    });
                }
                Open::Failed(error) => {
                    ui.colored_label(Color32::RED, error.clone());
                }
                Open::Ready(rendered) => {
                    if let Some(target) = layer::ui(ui, rendered, &mut self.deps, backend) {
                        follow = Some(target);
                    }
                }
            }
        });

        follow
    }
}

async fn load_index(excel: CachedProvider) -> Result<Vec<Zone>> {
    let sheet = excel.get_sheet("TerritoryType", Language::None).await?;
    let mut zones = Vec::new();
    for row_id in sheet.get_row_ids() {
        let Ok(row) = sheet.get_row(row_id) else {
            continue;
        };
        let Ok(bg) = row.read_string(BG) else {
            continue;
        };
        let bg = String::from_utf8_lossy(bg.as_bytes()).into_owned();
        if bg.is_empty() {
            continue;
        }
        let name = row
            .read_string(NAME)
            .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
            .unwrap_or_default();
        let place_name = row.read::<u16>(PLACE_NAME).unwrap_or_default();
        zones.push(Zone {
            place_name: u32::from(place_name),
            name,
            path: format!("bg/{bg}.lvb"),
        });
    }
    Ok(zones)
}

async fn load_names(excel: CachedProvider, language: Language) -> Result<HashMap<u32, String>> {
    let sheet = excel.get_sheet("PlaceName", language).await?;
    let mut names = HashMap::new();
    for row_id in sheet.get_row_ids() {
        let Ok(row) = sheet.get_row(row_id) else {
            continue;
        };
        let Ok(name) = row.read_string(0) else {
            continue;
        };
        let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
        if !name.is_empty() {
            names.insert(row_id, name);
        }
    }
    Ok(names)
}

async fn check_availability(
    files: Rc<dyn FileProvider>,
    paths: Vec<String>,
) -> Result<HashSet<String>> {
    let mut available = HashSet::with_capacity(paths.len());
    for chunk in paths.chunks(100) {
        let exists = files.exists_many(chunk).await?;
        for (path, ok) in chunk.iter().zip(exists) {
            if ok {
                available.insert(path.clone());
            }
        }
    }
    Ok(available)
}
