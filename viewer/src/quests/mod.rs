mod dag;
mod derive;
mod detail;
mod graph;
mod index;
mod tree;

use std::collections::HashSet;

use anyhow::Result;
use egui::{
    Align, Button, CentralPanel, Color32, Layout, RichText, ScrollArea, TextEdit, Vec2, Widget,
    containers::panel::Panel,
};
use ironworks::excel::Language;

use crate::{
    backend::Backend,
    goto::{ListNav, Palette, SUGGESTIONS},
    quests::{
        index::{Index, Loaded},
        tree::{Outline, Row},
    },
    settings::LANGUAGE,
    sheet::GlobalContext,
    utils::{CollapsibleSidePanel, FuzzyMatcher, IconManager, PromiseKind, Side, TrackedPromise},
};

const FILTER_ID: &str = "quest_filter";

pub enum Action {
    /// A quest was picked; reflect it in the URL.
    Select(u32),
    /// A link out of the tab: a sheet row, or a file for the asset browser.
    Navigate(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Journal,
    Chains,
}

#[derive(Default)]
enum Load<T: Send + 'static> {
    #[default]
    Idle,
    Loading(TrackedPromise<Result<T>>),
    Ready(T),
    Failed(String),
}

impl<T: Send + 'static> Load<T> {
    fn spawn(future: impl Future<Output = Result<T>> + 'static) -> Self {
        Self::Loading(TrackedPromise::spawn_local(future))
    }

    fn poll(&mut self) {
        if !matches!(self, Self::Loading(promise) if promise.try_get().is_some()) {
            return;
        }
        let Self::Loading(promise) = std::mem::replace(self, Self::Idle) else {
            unreachable!()
        };
        *self = match promise.block_and_take() {
            Ok(value) => Self::Ready(value),
            Err(error) => Self::Failed(error.to_string()),
        };
    }
}

pub struct QuestBrowser {
    /// Which language the index in hand or in flight was read for. `Quest` ships no
    /// `Language::None`, so this has to follow the app's setting or the tab shows nothing.
    loading_for: Option<Language>,
    loading: Option<TrackedPromise<Result<Loaded>>>,
    index: Option<Index>,
    outline: Option<Outline>,
    error: Option<String>,

    view: View,
    query: String,
    /// Which quests the query left, by node, and the same set ranked for the palette.
    matched: Vec<bool>,
    ranked: Vec<u32>,
    matched_for: Option<String>,
    show_uncategorized: bool,
    expanded: HashSet<u32>,
    rows: Vec<(Row, u32)>,
    rows_stale: bool,

    selected: Option<u32>,
    pending: Option<u32>,
    /// A quest the view has yet to bring on screen.
    reveal: Option<u32>,
    detail: detail::Detail,
    matcher: FuzzyMatcher,
    palette: Option<Palette>,
    nav: ListNav,
}

impl Default for QuestBrowser {
    fn default() -> Self {
        Self {
            loading_for: None,
            loading: None,
            index: None,
            outline: None,
            error: None,
            view: View::Journal,
            query: String::new(),
            matched: Vec::new(),
            ranked: Vec::new(),
            matched_for: None,
            show_uncategorized: false,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_stale: true,
            selected: None,
            pending: None,
            reveal: None,
            detail: detail::Detail::default(),
            matcher: FuzzyMatcher::new(),
            palette: None,
            nav: ListNav::default(),
        }
    }
}

impl QuestBrowser {
    pub fn selected(&self) -> Option<u32> {
        self.selected.or(self.pending)
    }

    /// The title a deep link should carry, once the quest it names has been read.
    pub fn name_of(&self, row_id: u32) -> Option<&str> {
        let index = self.index.as_ref()?;
        Some(index.quest(index.node_of(row_id)?).name.as_str())
    }

    /// Select the quest a deep link names, once there is an index to place it in.
    pub fn request(&mut self, row_id: u32) {
        if self.selected != Some(row_id) {
            self.pending = Some(row_id);
        }
    }

    /// Drop everything that came from the install, so a reconnect reads it all again.
    pub fn reset(&mut self) {
        self.loading_for = None;
        self.loading = None;
        self.index = None;
        self.outline = None;
        self.error = None;
        self.expanded.clear();
        self.matched_for = None;
        self.rows_stale = true;
        self.pending = self.pending.take().or(self.selected.take());
    }

    pub fn open_palette(&mut self) {
        self.palette = Some(Palette::new("Find Quest…", "Filter", self.query.clone()));
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        backend: &Backend,
        icons: &IconManager,
    ) -> Option<Action> {
        let language = LANGUAGE.get(ui.ctx());
        self.poll(ui, backend, icons, language);
        if let Some(row_id) = self.pending.take() {
            self.select(row_id);
        }
        self.rebuild(ui.ctx());

        let picked = self.draw_palette(ui.ctx());
        let listed = self.view == View::Journal && self.index.is_some();
        self.nav
            .claim(ui.ctx(), listed, Some(egui::Id::new(FILTER_ID)));

        let from_detail = self.detail_panel(ui, backend, language);
        let from_view = self.center(ui);

        let action = picked.map(Action::Select).or(from_detail).or(from_view);
        if let Some(Action::Select(row_id)) = &action {
            self.select(*row_id);
        }
        action
    }

    fn select(&mut self, row_id: u32) {
        self.selected = Some(row_id);
        self.reveal = Some(row_id);
        if let (Some(index), Some(outline)) = (&self.index, &self.outline)
            && let Some(node) = index.node_of(row_id)
        {
            self.expanded.extend(outline.path_to(node));
            self.rows_stale = true;
        }
    }

    fn poll(&mut self, ui: &egui::Ui, backend: &Backend, icons: &IconManager, language: Language) {
        if self.loading_for != Some(language) {
            self.loading_for = Some(language);
            self.index = None;
            self.outline = None;
            self.error = None;
            let backend = backend.clone();
            self.loading = Some(TrackedPromise::spawn_local(async move {
                index::load(backend, language).await
            }));
        }
        if self.loading.as_ref().is_some_and(|p| p.try_get().is_some()) {
            match self.loading.take().unwrap().block_and_take() {
                Ok(loaded) => {
                    let global = GlobalContext::new(
                        ui.ctx().clone(),
                        backend.clone(),
                        language,
                        icons.clone(),
                    );
                    let index = Index::new(global, loaded);
                    let outline = Outline::build(&index.sections, &index.uncategorized);
                    if self.expanded.is_empty() {
                        self.expanded.extend(
                            outline
                                .groups
                                .iter()
                                .enumerate()
                                .filter(|(_, group)| group.depth == 0)
                                .map(|(at, _)| at as u32),
                        );
                    }
                    self.outline = Some(outline);
                    self.index = Some(index);
                    self.matched_for = None;
                    self.rows_stale = true;
                    self.reveal = self.selected;
                    if let Some(row_id) = self.selected {
                        self.select(row_id);
                    }
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    /// Redo the filter and the tree rows, which only change on a keystroke or a toggle.
    fn rebuild(&mut self, ctx: &egui::Context) {
        let (Some(index), Some(outline)) = (&self.index, &self.outline) else {
            return;
        };
        if self.matched_for.as_ref() != Some(&self.query) {
            self.matched_for = Some(self.query.clone());
            self.rows_stale = true;
            if self.query.is_empty() {
                self.matched = vec![true; index.quests.len()];
                self.ranked = (0..index.quests.len() as u32).collect();
            } else {
                let pattern = FuzzyMatcher::parse_pattern(&self.query);
                let scores: Vec<Option<u32>> = index
                    .quests
                    .iter()
                    .map(|quest| {
                        let name = self.matcher.score_one(&pattern, &quest.name);
                        let id = self.matcher.score_one(&pattern, &quest.id);
                        name.max(id).map(Into::into)
                    })
                    .collect();
                self.matched = scores.iter().map(Option::is_some).collect();
                self.ranked = (0..index.quests.len() as u32)
                    .filter(|node| self.matched[*node as usize])
                    .collect();
                self.ranked
                    .sort_by_key(|node| std::cmp::Reverse(scores[*node as usize]));
            }
        }
        if std::mem::take(&mut self.rows_stale) {
            outline.rows(
                &self.expanded,
                &self.matched,
                self.show_uncategorized,
                &mut self.rows,
            );
            ctx.request_repaint();
        }
    }

    fn draw_palette(&mut self, ctx: &egui::Context) -> Option<u32> {
        let palette = self.palette.take()?;
        match palette.draw(ctx, |query| {
            self.query = query.to_owned();
            self.rebuild(ctx);
            let Some(index) = &self.index else {
                return Vec::new();
            };
            self.ranked
                .iter()
                .take(SUGGESTIONS)
                .map(|node| {
                    let quest = index.quest(*node);
                    (quest.row_id, quest.name.clone())
                })
                .collect()
        }) {
            Ok(picked) => picked,
            Err(palette) => {
                self.palette = Some(palette);
                None
            }
        }
    }

    fn center(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        CentralPanel::default().show(ui, |ui| {
            Panel::top("quest_header").show(ui, |ui| {
                ui.add_space(4.0);
                self.header(ui);
                ui.add_space(4.0);
            });

            CentralPanel::default().show(ui, |ui| {
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::RED, error.clone());
                    return;
                }
                if self.index.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading quests…");
                    });
                    return;
                }
                action = match self.view {
                    View::Journal => self.draw_tree(ui),
                    View::Chains => self.draw_chains(ui),
                };
            });
        });
        action
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x /= 2.0;
            for (view, glyph, hover) in [
                (View::Journal, "📖", "Journal"),
                (View::Chains, "🕸", "Prerequisite chains"),
            ] {
                if ui
                    .add(Button::selectable(self.view == view, glyph))
                    .on_hover_text(hover)
                    .clicked()
                {
                    self.view = view;
                    self.reveal = self.selected;
                }
            }
            if self.view == View::Journal
                && ui
                    .toggle_value(&mut self.show_uncategorized, "👁")
                    .on_hover_text("Show quests with no journal entry")
                    .changed()
            {
                self.rows_stale = true;
            }
            if ui
                .add_enabled(!self.query.is_empty(), Button::new("↩"))
                .on_hover_text("Clear")
                .clicked()
            {
                self.query.clear();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if CollapsibleSidePanel::is_collapsed(ui.ctx(), "quest_info") {
                    CollapsibleSidePanel::draw_arrow(ui, "quest_info", Side::Right);
                }
                ui.label(RichText::new(self.summary()).weak());
                ui.add_sized(
                    Vec2::new(ui.available_width().min(320.0), 0.0),
                    TextEdit::singleline(&mut self.query)
                        .id(egui::Id::new(FILTER_ID))
                        .hint_text("Filter"),
                );
            });
        });
    }

    fn summary(&self) -> String {
        let Some(index) = &self.index else {
            return String::new();
        };
        match self.view {
            View::Journal => format!(
                "{} of {} quests",
                self.matched.iter().filter(|hit| **hit).count(),
                index.quests.len()
            ),
            View::Chains => {
                let component = self
                    .selected
                    .and_then(|row_id| index.node_of(row_id))
                    .map_or(0, |node| index.graph.component(node));
                format!(
                    "chain {} of {} · {} quests · {} steps",
                    component + 1,
                    index.graph.component_count(),
                    index.graph.component_nodes(component).len(),
                    index.graph.extent(component).0 + 1
                )
            }
        }
    }

    fn draw_chains(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let index = self.index.as_ref()?;
        dag::ui(
            ui,
            index,
            self.selected,
            &self.matched,
            self.query.is_empty(),
            &mut self.reveal,
        )
        .map(Action::Select)
    }

    fn draw_tree(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let Self {
            index: Some(index),
            outline: Some(outline),
            rows,
            expanded,
            nav,
            reveal,
            rows_stale,
            selected,
            ..
        } = self
        else {
            return None;
        };

        let height = ui.text_style_height(&egui::TextStyle::Button);
        let mut picked = nav.apply(rows.len()).and_then(|at| match rows[at].0 {
            Row::Quest { node, .. } => Some(index.quest(node).row_id),
            Row::Group(_) => None,
        });

        let scroll_to = reveal.take().and_then(|row_id| {
            let node = index.node_of(row_id)?;
            rows.iter()
                .position(|(row, _)| matches!(row, Row::Quest { node: at, .. } if *at == node))
        });
        let mut area = ScrollArea::vertical().auto_shrink(false);
        if let Some(at) = scroll_to {
            let pitch = height + ui.spacing().item_spacing.y;
            let last = (rows.len() as f32 * pitch - ui.available_height()).max(0.0);
            area = area.vertical_scroll_offset(
                (at as f32 * pitch - ui.available_height() / 2.0).clamp(0.0, last),
            );
        } else if let Some(offset) = nav.scroll(ui, height, rows.len()) {
            area = area.vertical_scroll_offset(offset);
        }

        let mut toggled = None;
        let output = area.show_rows(ui, height, rows.len(), |ui, range| {
            ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                for (at, (row, count)) in rows[range.clone()].iter().enumerate() {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    let response = match row {
                        Row::Group(group) => {
                            let open = expanded.contains(group);
                            let group = &outline.groups[*group as usize];
                            indented(
                                ui,
                                f32::from(group.depth),
                                Button::selectable(
                                    false,
                                    format!(
                                        "{} {} ({count})",
                                        if open { "⏷" } else { "⏵" },
                                        group.label
                                    ),
                                ),
                            )
                        }
                        Row::Quest { node, depth } => {
                            let quest = index.quest(*node);
                            indented(
                                ui,
                                f32::from(*depth),
                                Button::selectable(
                                    *selected == Some(quest.row_id),
                                    quest.name.as_str(),
                                ),
                            )
                        }
                    };
                    nav.mark(ui, range.start + at, response.rect);
                    if response.clicked() {
                        match row {
                            Row::Group(group) => toggled = Some(*group),
                            Row::Quest { node, .. } => picked = Some(index.quest(*node).row_id),
                        }
                    }
                }
            });
        });
        nav.seen(&output);

        if let Some(group) = toggled {
            if !expanded.remove(&group) {
                expanded.insert(group);
            }
            *rows_stale = true;
        }
        picked.map(Action::Select)
    }

    fn detail_panel(
        &mut self,
        ui: &mut egui::Ui,
        backend: &Backend,
        language: Language,
    ) -> Option<Action> {
        let node = self
            .selected
            .zip(self.index.as_ref())
            .and_then(|(row_id, index)| index.node_of(row_id));
        if let (Some(node), Some(index)) = (node, &self.index) {
            self.detail.poll(backend, index, node, language);
        }

        let mut action = None;
        CollapsibleSidePanel::new("quest_info", Side::Right)
            .collapsed_width(0.0)
            .show(ui, |ui, is_open| {
                if !is_open {
                    return;
                }
                let title = node
                    .zip(self.index.as_ref())
                    .map_or("Quest", |(node, index)| index.quest(node).name.as_str());
                Panel::top("quest_info_header").show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        CollapsibleSidePanel::draw_arrow(ui, "quest_info", Side::Right);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // Balances the arrow, so the heading centers on the panel rather than
                            // on the space left over beside it.
                            ui.add_space(ui.spacing().indent);
                            ui.vertical_centered_justified(|ui| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                ui.heading(title);
                            });
                        });
                    });
                    ui.add_space(4.0);
                });

                CentralPanel::default().show(ui, |ui| {
                    let (Some(node), Some(index)) = (node, &self.index) else {
                        ui.centered_and_justified(|ui| {
                            ui.label(RichText::new("No quest selected").weak());
                        });
                        return;
                    };
                    action = self.detail.ui(ui, index, node).map(|action| match action {
                        detail::Action::Select(row_id) => Action::Select(row_id),
                        detail::Action::Navigate(route) => Action::Navigate(route),
                    });
                });
            });
        action
    }
}

/// A row that starts at its depth and fills the rest of the line, so the whole strip is clickable.
fn indented(ui: &mut egui::Ui, depth: f32, button: Button<'_>) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(depth * ui.spacing().indent);
        ui.with_layout(Layout::top_down_justified(Align::Min), |ui| button.ui(ui))
            .inner
    })
    .inner
}
