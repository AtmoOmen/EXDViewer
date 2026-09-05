//! The grass a zone grows: `.gzd` indexes the grids covering it, and each `.ggd` holds the
//! placements inside one of them.
//!
//! Only the `.gzd` names anything: a `.ggd` is filed under its cell and level of detail, so the
//! links out of the zone file are the only way to reach one by name.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::Result;
use egui::{Color32, RichText, ScrollArea, Vec2, load::SizedTexture};
use ironworks::file::{File, ggd, gzd};

use super::{Preview, facts, heading, line, link, placed, section, table};
use crate::assets::deps::{Dep, Deps};
use crate::backend::Backend;
use crate::utils::file_name;

/// Longest edge a color map is drawn at beside its name.
const THUMBNAIL: f32 = 64.0;

/// Placements one grid draws. The whole instance buffer is written every frame, so a grid past
/// this many is shown thinned rather than in full.
const DRAWN: usize = 60_000;

const GRIDS: [(&str, usize); 4] = [("细节", 7), ("单元格", 12), ("中心", 26), ("半径", 10)];

const PLACEMENTS: [(&str, usize); 7] = [
    ("区块", 6),
    ("图层", 9),
    ("位置", 26),
    ("旋转", 34),
    ("缩放", 16),
    ("风相位", 7),
    ("配置", 8),
];

/// What a chunk's count slot holds: the leading few are procedural grass, the rest name a model
/// the zone's `.gzd` lists, and the last is unused.
fn layer(slot: usize) -> String {
    match slot.checked_sub(ggd::Chunk::AUTO_LAYERS) {
        Some(model) => format!("模型 {model}"),
        None => format!("自动 {slot}"),
    }
}

fn axes(values: [f32; 3]) -> String {
    let [x, y, z] = values.map(|value| format!("{value:.3}"));
    format!("{x:>8} {y:>8} {z:>8}")
}

/// A `.gzd`, decoded and ready to draw.
pub struct Zone {
    identity: Vec<(&'static str, String)>,
    /// The grids in space, built on the first switch to the scene.
    scene: RefCell<Option<placed::View>>,
    /// Whether the tables or the scene is showing.
    placed: Cell<bool>,
    /// Where the zone's own directory sits, which is what its names are relative to.
    directory: String,
    color_maps: Vec<String>,
    models: Vec<String>,
    /// Level of detail and index of every grid.
    rows: Vec<(gzd::Detail, usize)>,
    file: gzd::GrassZone,
}

/// A `.ggd`, decoded and ready to draw.
pub struct Grid {
    identity: Vec<(&'static str, String)>,
    /// Chunk, index within it, and the count slot it was stored under, for every placement.
    rows: Vec<(usize, usize, usize)>,
    file: ggd::GrassGrid,
    /// Every placement in space, built on the first switch to the scene.
    scene: RefCell<Option<placed::View>>,
    /// Whether the table or the scene is showing.
    placed: Cell<bool>,
    /// How many placements the scene left out, where it thinned them.
    dropped: usize,
}

pub fn zone(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = gzd::GrassZone::read(Cursor::new(bytes.to_vec()))?;
    let details = [gzd::Detail::High, gzd::Detail::Medium, gzd::Detail::Low];
    let rows = details
        .into_iter()
        .flat_map(|detail| (0..file.grids(detail).len()).map(move |index| (detail, index)))
        .collect::<Vec<_>>();

    let directory = path
        .rsplit_once('/')
        .map_or("", |(head, _)| head)
        .to_owned();
    let identity = vec![
        ("版本", format!("{:#010x}", file.version())),
        ("网格", rows.len().to_string()),
        (
            "各细节层级",
            details
                .map(|detail| file.grids(detail).len().to_string())
                .join(" / "),
        ),
        ("模型槽位", file.model_slot_capacity().to_string()),
        (
            "图层值",
            file.auto_layer_values().map_or_else(
                || "无".to_owned(),
                |values| values.map(|value| format!("{value:.2}")).join(", "),
            ),
        ),
    ];

    log::info!("assets/grass: {path} {} 个网格", rows.len());

    Ok(Preview::GrassZone(Box::new(Zone {
        identity,
        scene: RefCell::new(None),
        placed: Cell::new(false),
        color_maps: file
            .color_map()
            .iter()
            .filter(|name| !name.is_empty())
            .map(|name| format!("{directory}/{name}.tex"))
            .collect(),
        models: file.model_paths().to_vec(),
        directory,
        rows,
        file,
    })))
}

pub fn grid(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = ggd::GrassGrid::read(Cursor::new(bytes.to_vec()))?;
    let rows = file
        .chunks()
        .iter()
        .enumerate()
        .flat_map(|(chunk, held)| {
            held.counts()
                .iter()
                .enumerate()
                .flat_map(move |(slot, count)| (0..usize::from(*count)).map(move |_| slot))
                .enumerate()
                .map(move |(index, slot)| (chunk, index, slot))
        })
        .collect::<Vec<_>>();

    let identity = vec![
        ("版本", format!("{:#010x}", file.version())),
        ("区块", file.chunks().len().to_string()),
        ("放置", rows.len().to_string()),
        ("世界原点", axes(file.world_origin())),
        (
            "对齐",
            file.alignment_bend_weight().map_or_else(
                || "无".to_owned(),
                |weights| weights.map(|it| it.to_string()).join(", "),
            ),
        ),
    ];

    log::info!("assets/grass: {path} {} 个放置", rows.len());

    let step = rows.len().div_ceil(DRAWN).max(1);
    Ok(Preview::GrassGrid(Box::new(Grid {
        identity,
        dropped: rows.len() - rows.len().div_ceil(step),
        rows,
        file,
        scene: RefCell::new(None),
        placed: Cell::new(false),
    })))
}

/// A texture drawn at thumbnail size, or the room it will take once it arrives.
fn thumbnail(ui: &mut egui::Ui, deps: &mut Deps, backend: &Backend, path: &str) {
    match deps.texture(ui.ctx(), backend, path) {
        Dep::Ready(handle) => {
            let size = handle.size_vec2();
            let scale = THUMBNAIL / size.x.max(size.y).max(1.0);
            ui.add(
                egui::Image::new(SizedTexture::new(handle, size * scale))
                    .maintain_aspect_ratio(true),
            );
        }
        Dep::Pending => {
            ui.add_sized(
                Vec2::splat(THUMBNAIL),
                egui::Spinner::new().size(THUMBNAIL / 2.0),
            );
        }
        Dep::Failed => {
            ui.add_sized(
                Vec2::splat(THUMBNAIL),
                egui::Label::new(RichText::new("⚠").color(Color32::LIGHT_RED)),
            )
            .on_hover_text("加载失败");
        }
    }
}

pub fn zone_ui(
    ui: &mut egui::Ui,
    file: &Zone,
    deps: &mut Deps,
    backend: &Backend,
) -> Option<String> {
    let mut follow = None;
    ui.horizontal(|ui| {
        for (scene, label) in [(false, "文件"), (true, "场景")] {
            if ui
                .selectable_label(file.placed.get() == scene, label)
                .clicked()
            {
                file.placed.set(scene);
            }
        }
    });
    ui.add_space(4.0);

    if file.placed.get() {
        let mut held = file.scene.borrow_mut();
        held.get_or_insert_with(|| file.build()).ui(ui);
        return None;
    }

    if !file.color_maps.is_empty() {
        section(ui, "颜色贴图");
        ui.horizontal_wrapped(|ui| {
            for path in &file.color_maps {
                ui.vertical(|ui| {
                    thumbnail(ui, deps, backend, path);
                    if link(ui, file_name(path), path) {
                        follow = Some(path.clone());
                    }
                });
            }
        });
        ui.add_space(4.0);
    }

    if !file.models.is_empty() {
        section(ui, "模型");
        for path in &file.models {
            if link(ui, file_name(path), path) {
                follow = Some(path.clone());
            }
        }
        ui.add_space(4.0);
    }

    section(ui, "网格");
    let mut picked = None;
    table(ui, &GRIDS, file.rows.len(), |ui, index| {
        let (detail, at) = file.rows[index];
        let grid = file.file.grids(detail)[at];
        let cells = [
            format!("{detail:?}"),
            grid.cell().map(|it| it.to_string()).join(", "),
            axes(grid.center()),
            format!("{:.3}", grid.radius()),
        ];
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new(line(&GRIDS, cells.iter().map(String::as_str))).monospace());
            let path = format!("{}/{}", file.directory, grid.file());
            if link(ui, &grid.file(), &path) {
                picked = Some(path);
            }
        });
    });
    follow.or(picked)
}

pub fn grid_ui(ui: &mut egui::Ui, file: &Grid) {
    ui.horizontal(|ui| {
        for (scene, label) in [(false, "表格"), (true, "场景")] {
            if ui
                .selectable_label(file.placed.get() == scene, label)
                .clicked()
            {
                file.placed.set(scene);
            }
        }
        if file.placed.get() && file.dropped > 0 {
            ui.label(RichText::new(format!("已精简 {} 个放置", file.dropped)).weak());
        }
    });
    ui.add_space(4.0);

    if file.placed.get() {
        let mut held = file.scene.borrow_mut();
        held.get_or_insert_with(|| file.build()).ui(ui);
        return;
    }

    section(ui, "放置");
    table(ui, &PLACEMENTS, file.rows.len(), |ui, index| {
        let (chunk, at, slot) = file.rows[index];
        let placement = file.file.chunks()[chunk].placements()[at];
        let [x, y, z, w] = placement.rotation().map(|value| format!("{value:.3}"));
        let cells = [
            chunk.to_string(),
            layer(slot),
            axes(placement.position()),
            format!("{x:>8} {y:>8} {z:>8} {w:>8}"),
            format!("{:.3} x {:.3}", placement.scale_xz(), placement.scale_y()),
            format!("{:.3}", placement.wind_phase()),
            placement.profile().to_string(),
        ];
        ui.label(RichText::new(line(&PLACEMENTS, cells.iter().map(String::as_str))).monospace());
    });
}

impl Zone {
    /// Every grid as the sphere it is culled by, one color per level of detail, so the coverage
    /// of a zone reads at a glance.
    fn build(&self) -> placed::View {
        let batches = [gzd::Detail::High, gzd::Detail::Medium, gzd::Detail::Low]
            .into_iter()
            .enumerate()
            .map(|(index, detail)| placed::Batch {
                shape: placed::Shape::Wire,
                instances: self
                    .file
                    .grids(detail)
                    .iter()
                    .map(|grid| placed::Instance {
                        center: grid.center(),
                        scale: [grid.radius(); 3],
                        turn: [0.0, 0.0, 0.0, 1.0],
                        color: placed::tint(index * 3),
                    })
                    .collect(),
            })
            .collect();
        placed::View::new(batches)
    }

    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "gzd_identity", &self.identity));
    }
}

impl Grid {
    /// Every placement as a box at the size and turn it was stored with, colored by the layer it
    /// came from, and moved out to the world origin its grid is measured from.
    fn build(&self) -> placed::View {
        let origin = glam::Vec3::from_array(self.file.world_origin());
        let step = self.rows.len().div_ceil(DRAWN).max(1);
        let mut batches: BTreeMap<usize, Vec<placed::Instance>> = BTreeMap::new();
        for &(chunk, at, slot) in self.rows.iter().step_by(step) {
            let placement = self.file.chunks()[chunk].placements()[at];
            let position = origin + glam::Vec3::from_array(placement.position());
            let [x, z] = [placement.scale_xz(), placement.scale_xz()];
            batches.entry(slot).or_default().push(placed::Instance {
                center: position.to_array(),
                scale: [x * 0.5, placement.scale_y() * 0.5, z * 0.5],
                turn: placement.rotation(),
                color: placed::tint(slot),
            });
        }
        placed::View::new(
            batches
                .into_values()
                .map(|instances| placed::Batch {
                    shape: placed::Shape::Box,
                    instances,
                })
                .collect(),
        )
    }

    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            facts(ui, "ggd_identity", &self.identity);
            heading(ui, "区块");
            let rows = self
                .file
                .chunks()
                .iter()
                .enumerate()
                .map(|(index, chunk)| {
                    (
                        index.to_string(),
                        format!("{} 个放置", chunk.placements().len()),
                    )
                })
                .collect::<Vec<_>>();
            egui::Grid::new("ggd_chunks")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for (index, count) in rows {
                        ui.label(RichText::new(index).weak());
                        ui.label(RichText::new(count).monospace());
                        ui.end_row();
                    }
                });
        });
    }
}
