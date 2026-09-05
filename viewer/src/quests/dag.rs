use egui::{Color32, Rect, ScrollArea, Sense, Shape, Stroke, Vec2, pos2, text::LayoutJob, vec2};

use crate::quests::{graph::Graph, index::Index};

/// One rank per column, one slot per row. Ranks are the long axis, since a chain reaches much
/// further than any one rank is wide.
const RANK: f32 = 240.0;
const SLOT: f32 = 28.0;
const NODE: Vec2 = vec2(206.0, 22.0);

pub fn ui(
    ui: &mut egui::Ui,
    index: &Index,
    selected: Option<u32>,
    matched: &[bool],
    unfiltered: bool,
    reveal: &mut Option<u32>,
) -> Option<u32> {
    let graph = &index.graph;
    let focus = selected.and_then(|row_id| index.node_of(row_id));
    let component = focus.map_or(0, |node| graph.component(node));
    let (last_rank, last_slot) = graph.extent(component);
    let content = vec2((last_rank + 1) as f32 * RANK, (last_slot + 1) as f32 * SLOT);

    let mut area = ScrollArea::both().auto_shrink(false);
    if let Some(node) = reveal.take().and_then(|row_id| index.node_of(row_id)) {
        let at = at_of(Vec2::ZERO, graph, node);
        area = area
            .horizontal_scroll_offset((at.center().x - ui.available_width() / 2.0).max(0.0))
            .vertical_scroll_offset((at.center().y - ui.available_height() / 2.0).max(0.0));
    }

    // A component holds every branch anyone ever took to reach a quest; the chain through the one
    // in hand is the part of it that answers what leads here and what it opens.
    let chain = focus.map(|node| graph.chain(node));
    let on_chain = |node: u32| chain.as_ref().is_none_or(|held| held[node as usize]);

    let mut picked = None;
    area.show_viewport(ui, |ui, viewport| {
        let (canvas, _) = ui.allocate_exact_size(content, Sense::hover());
        let origin = canvas.min.to_vec2();
        let first = (viewport.min.x / RANK) as u32;
        let last = ((viewport.max.x / RANK) as u32 + 2).min(last_rank + 1);
        let visible = graph.ranked_slice(component, first..last);

        let painter = ui.painter();
        let faint = ui.visuals().weak_text_color().gamma_multiply(0.6);
        let lit = ui.visuals().text_color();
        for node in visible {
            let to = at_of(origin, graph, *node);
            // An OR-joined set needs any one of its quests, so those edges are drawn apart from
            // the ones that are really required.
            let optional = index.quest(*node).join == 2 && graph.prereqs(*node).len() > 1;
            for prereq in graph.prereqs(*node) {
                let color = match on_chain(*node) && on_chain(*prereq) {
                    true => lit,
                    false => faint,
                };
                edge(painter, at_of(origin, graph, *prereq), to, color, optional);
            }
            for dependent in graph.dependents(*node) {
                if !(first..last).contains(&graph.rank(*dependent)) {
                    let dependent_optional =
                        index.quest(*dependent).join == 2 && graph.prereqs(*dependent).len() > 1;
                    edge(
                        painter,
                        to,
                        at_of(origin, graph, *dependent),
                        match on_chain(*node) && on_chain(*dependent) {
                            true => lit,
                            false => faint,
                        },
                        dependent_optional,
                    );
                }
            }
        }

        for node in visible {
            let rect = at_of(origin, graph, *node);
            let quest = index.quest(*node);
            let chosen = focus == Some(*node);
            let hit = (unfiltered || matched[*node as usize]) && on_chain(*node);
            let response = ui
                .interact(rect, ui.id().with(*node), Sense::click())
                .on_hover_text(format!(
                    "{}\n{} · 步骤 {}",
                    quest.name,
                    quest.id,
                    graph.rank(*node) + 1
                ))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                picked = Some(quest.row_id);
            }

            let visuals = ui.visuals();
            let fill = if chosen {
                visuals.selection.bg_fill
            } else if response.hovered() {
                visuals.widgets.hovered.bg_fill
            } else {
                visuals.extreme_bg_color
            };
            let stroke = if chosen {
                visuals.selection.stroke
            } else {
                Stroke::new(1.0, faint)
            };
            painter.rect(rect, 3.0, fill, stroke, egui::StrokeKind::Inside);

            let color = if chosen {
                visuals.strong_text_color()
            } else if hit {
                visuals.text_color()
            } else {
                visuals.weak_text_color().gamma_multiply(0.5)
            };
            let mut job = LayoutJob::simple(
                quest.name.clone(),
                egui::TextStyle::Body.resolve(ui.style()),
                color,
                NODE.x - 10.0,
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            painter.galley(
                pos2(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
        }
    });
    picked
}

fn at_of(origin: Vec2, graph: &Graph, node: u32) -> Rect {
    Rect::from_min_size(
        pos2(
            graph.rank(node) as f32 * RANK,
            graph.slot(node) as f32 * SLOT,
        ) + origin,
        NODE,
    )
}

fn edge(painter: &egui::Painter, from: Rect, to: Rect, color: Color32, optional: bool) {
    let points = [
        pos2(from.right(), from.center().y),
        pos2(to.left(), to.center().y),
    ];
    let stroke = Stroke::new(1.0, color);
    if optional {
        painter.extend(Shape::dashed_line(&points, stroke, 4.0, 4.0));
    } else {
        painter.line_segment(points, stroke);
    }
}
