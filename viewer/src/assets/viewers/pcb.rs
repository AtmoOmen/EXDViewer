//! Collision `.pcb` files, drawn and tabulated.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use egui::{Color32, RichText, ScrollArea};
use glam::Vec3;
use ironworks::file::{File, pcb};

use crate::backend::Backend;
use crate::data::FileProvider;
use crate::utils::TrackedPromise;

use super::{Preview, chip, facts, line, section, table};

mod gpu;

/// Sub-meshes a list may have in flight at once.
const FETCHES: usize = 12;

/// What the game's own collision swatches paint a surface, out of the `g_DiffuseColor` each of
/// `bgcommon/collision/material/id_*.mtrl` states. Nothing paints a surface the game does not
/// name, nor the sticky one.
fn paint(surface: u8) -> Option<[u8; 3]> {
    Some(match surface {
        1 => [151, 101, 6],
        2 => [134, 193, 82],
        3 => [247, 185, 70],
        4 => [141, 141, 89],
        5 => [255, 102, 255],
        6 => [255, 0, 0],
        7 => [218, 218, 196],
        8 => [67, 101, 36],
        9 => [251, 227, 181],
        10 => [112, 48, 160],
        11 => [198, 217, 240],
        12 => [84, 141, 212],
        13 => [31, 73, 125],
        14 => [255, 255, 0],
        _ => return None,
    })
}

/// The grey a surface no swatch paints is drawn in.
const UNPAINTED: [u8; 3] = [184, 189, 199];

fn color(material: u64) -> [u8; 3] {
    paint((material & 0xff) as u8).unwrap_or(UNPAINTED)
}

/// How many triangles state each material word. The word is kept whole rather than reduced to its
/// surface, so a mesh read at the wrong material width shows as the impossible word it is.
type Materials = BTreeMap<u64, usize>;

fn axes(values: [f32; 3]) -> String {
    format!("{:.3}, {:.3}, {:.3}", values[0], values[1], values[2])
}

pub struct Rendered {
    path: String,
    identity: Vec<(&'static str, String)>,
    show_scene: Cell<bool>,
    scene: RefCell<MeshScene>,
    loader: RefCell<Option<Loader>>,
    show_bounds: Cell<bool>,
    section: &'static str,
    columns: Vec<(&'static str, usize)>,
    rows: Vec<Vec<String>>,
    entries: Vec<pcb::MeshListEntry>,
    /// A list fills this in as its sub-meshes land, so it grows while the scene does.
    materials: RefCell<Materials>,
}

#[derive(Clone, Copy)]
struct Camera {
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: Vec3,
}

impl Camera {
    fn eye(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        self.target
            + Vec3::new(
                self.distance * cos_pitch * sin_yaw,
                self.distance * sin_pitch,
                self.distance * cos_pitch * cos_yaw,
            )
    }
}

struct MeshScene {
    renderer: Arc<Mutex<gpu::Renderer>>,
    camera: Cell<Camera>,
    home: Camera,
    reach: f32,
    triangles: Cell<usize>,
    /// A list's sub-meshes still to fetch, so an empty viewport before any of them land reads as
    /// loading rather than a file that draws nothing. Zero for a mesh drawn outright.
    meshes: usize,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    match pcb::Collision::read(Cursor::new(bytes.to_vec()))? {
        pcb::Collision::Mesh(mesh) => Ok(Preview::Pcb(Box::new(render_mesh(path, mesh)))),
        pcb::Collision::List(list) => Ok(Preview::Pcb(Box::new(render_list(path, list)))),
    }
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered, backend: &Backend) -> Option<String> {
    let follow = None;
    ui.horizontal(|ui| {
        if ui
            .selectable_label(!file.show_scene.get(), "表格")
            .clicked()
        {
            file.show_scene.set(false);
        }
        if ui
            .selectable_label(file.show_scene.get(), "场景")
            .clicked()
        {
            file.show_scene.set(true);
        }
    });
    ui.add_space(4.0);

    if file.show_scene.get() {
        let (loaded, total) = file.ensure_scene(backend, ui.ctx());
        if loaded < total {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("正在加载网格: {loaded} / {total}")).weak());
            });
            ui.add_space(4.0);
        }
        file.scene.borrow().ui(ui, file.show_bounds.get());
        return follow;
    }

    section(ui, file.section);
    table(ui, &file.columns, file.rows.len(), |ui, index| {
        let cells = file.rows[index].iter().map(String::as_str);
        ui.label(RichText::new(line(&file.columns, cells)).monospace());
    });

    follow
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        let mut show_bounds = self.show_bounds.get();
        ui.checkbox(&mut show_bounds, "显示线框包围盒");
        self.show_bounds.set(show_bounds);
        ui.add_space(8.0);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            facts(ui, "pcb_identity", &self.identity);
            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("表面").weak());
            ui.add_space(4.0);
            legend(ui, &self.materials.borrow(), self.entries.is_empty());
        });
    }

    /// Starts fetching a list's sub-meshes on first use and folds in whatever has since arrived.
    /// Returns how many of them are in, so the caller can show that while it's incomplete.
    fn ensure_scene(&self, backend: &Backend, ctx: &egui::Context) -> (usize, usize) {
        if self.entries.is_empty() {
            return (0, 0);
        }
        let mut loader = self.loader.borrow_mut();
        let loader = loader.get_or_insert_with(|| {
            Loader::new(backend.files().clone(), &self.path, self.entries.clone())
        });
        loader.poll(ctx, &self.scene.borrow(), &self.materials);
        (loader.loaded(), self.entries.len())
    }
}

fn render_mesh(path: &str, mesh: pcb::Mesh) -> Rendered {
    let root = mesh.root();
    let mut rows = Vec::new();
    let mut nodes = 0usize;
    let mut leaves = 0usize;
    let mut vertices = 0usize;
    let mut primitives = 0usize;
    let mut geometry = gpu::Geometry::new();

    collect_node(
        root,
        &mut Vec::new(),
        0,
        &mut rows,
        &mut nodes,
        &mut leaves,
        &mut vertices,
        &mut primitives,
    );
    collect_geometry(root, &mut geometry);
    let mut materials = Materials::new();
    collect_materials(root, &mut materials);

    let identity = vec![
        ("版本", mesh.version().to_string()),
        ("节点", nodes.to_string()),
        ("叶节点", leaves.to_string()),
        ("顶点", vertices.to_string()),
        ("三角形", primitives.to_string()),
        ("根最小", axes(root.bounds().min())),
        ("根最大", axes(root.bounds().max())),
    ];

    log::info!("assets/pcb: {path} {nodes} 个节点，{primitives} 个三角形");

    let scene = MeshScene::new((geometry.bounds.0, geometry.bounds.1), 0);
    scene.queue(Arc::new(geometry));

    Rendered {
        path: path.to_owned(),
        identity,
        show_scene: Cell::new(false),
        scene: RefCell::new(scene),
        loader: RefCell::new(None),
        show_bounds: Cell::new(false),
        section: "节点",
        columns: vec![
            ("深度", 5),
            ("路径", 12),
            ("顶点", 9),
            ("图元", 10),
            ("子节点", 8),
            ("表面", 16),
            ("最小", 26),
            ("最大", 26),
        ],
        rows,
        entries: Vec::new(),
        materials: RefCell::new(materials),
    }
}

fn render_list(path: &str, list: pcb::MeshList) -> Rendered {
    let rows = list
        .entries()
        .iter()
        .map(|entry| {
            vec![
                pcb::MeshList::mesh_file(entry.id()),
                axes(entry.bounds().min()),
                axes(entry.bounds().max()),
            ]
        })
        .collect::<Vec<_>>();
    let identity = vec![
        ("条目", list.entries().len().to_string()),
        ("最小", axes(list.bounds().min())),
        ("最大", axes(list.bounds().max())),
    ];

    log::info!("assets/pcb: {path} {} 个条目", list.entries().len());

    let scene = MeshScene::new(
        (list.bounds().min(), list.bounds().max()),
        list.entries().len(),
    );

    Rendered {
        path: path.to_owned(),
        identity,
        show_scene: Cell::new(false),
        scene: RefCell::new(scene),
        loader: RefCell::new(None),
        show_bounds: Cell::new(false),
        section: "网格",
        columns: vec![("网格", 12), ("最小", 26), ("最大", 26)],
        rows,
        entries: list.entries().to_vec(),
        materials: RefCell::new(Materials::new()),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_node(
    node: &pcb::Node,
    path: &mut Vec<usize>,
    depth: usize,
    rows: &mut Vec<Vec<String>>,
    nodes: &mut usize,
    leaves: &mut usize,
    vertices: &mut usize,
    primitives: &mut usize,
) {
    let bounds = node.bounds();
    rows.push(vec![
        depth.to_string(),
        if path.is_empty() {
            "根".to_owned()
        } else {
            path.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(".")
        },
        node.vertices().len().to_string(),
        node.primitives().len().to_string(),
        node.children().len().to_string(),
        surfaces(node),
        axes(bounds.min()),
        axes(bounds.max()),
    ]);

    *nodes += 1;
    *vertices += node.vertices().len();
    *primitives += node.primitives().len();
    if node.children().is_empty() {
        *leaves += 1;
    }

    for (index, child) in node.children().iter().enumerate() {
        path.push(index);
        collect_node(
            child,
            path,
            depth + 1,
            rows,
            nodes,
            leaves,
            vertices,
            primitives,
        );
        path.pop();
    }
}

/// What the mesh says its surfaces are, which is also the key to the colours the scene draws them
/// in. `whole` is false for a list, whose sub-meshes arrive one at a time.
fn legend(ui: &mut egui::Ui, materials: &Materials, whole: bool) {
    if materials.is_empty() {
        let text = match whole {
            true => "此网格未声明材质",
            false => "尚未加载到可声明的材质",
        };
        ui.label(RichText::new(text).weak());
        return;
    }
    if materials.keys().all(|word| *word == 0) {
        ui.label(RichText::new("此处的每个三角形都未声明材质" ).weak());
        return;
    }

    let mut held: BTreeMap<u8, (usize, BTreeSet<u64>)> = BTreeMap::new();
    for (word, count) in materials {
        let row = held.entry((word & 0xff) as u8).or_default();
        row.0 += count;
        row.1.insert(word & !0xff);
    }
    let mut rows: Vec<_> = held.into_iter().collect();
    rows.sort_by_key(|(surface, (count, _))| (std::cmp::Reverse(*count), *surface));

    egui::Grid::new("pcb_surfaces")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            for (surface, (count, flags)) in rows {
                let [red, green, blue] = paint(surface).unwrap_or(UNPAINTED);
                chip(ui, Color32::from_rgb(red, green, blue))
                    .on_hover_text(match paint(surface) {
                        Some(_) => "游戏自身色板为其涂上的颜色",
                        None => "没有色板为其涂色",
                    });
                ui.label(match pcb::surface(u64::from(surface)) {
                    Some(name) => RichText::new(name),
                    None => RichText::new(format!("{surface:#04x}")).monospace(),
                });
                ui.label(RichText::new(count.to_string()).monospace());
                ui.label(RichText::new(masks(&flags)).monospace());
                ui.end_row();
            }
        });
}

/// The filter flags a surface is carrying, which only the `0x7000` group is understood in.
fn masks(flags: &BTreeSet<u64>) -> String {
    let mut held: Vec<String> = flags
        .iter()
        .take(4)
        .map(|mask| format!("{mask:#x}"))
        .collect();
    if flags.len() > 4 {
        held.push(format!("+{}", flags.len() - 4));
    }
    held.join(" ")
}

/// The surfaces one node's own triangles state, for the row that stands for it.
fn surfaces(node: &pcb::Node) -> String {
    let mut held: Vec<u8> = node
        .primitives()
        .iter()
        .map(|primitive| (primitive.material() & 0xff) as u8)
        .collect();
    held.sort_unstable();
    held.dedup();
    held.iter()
        .map(|surface| match pcb::surface(u64::from(*surface)) {
            Some(name) => name.to_owned(),
            None => format!("{surface:#04x}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn collect_materials(node: &pcb::Node, into: &mut Materials) {
    for primitive in node.primitives() {
        *into.entry(primitive.material()).or_default() += 1;
    }
    for child in node.children() {
        collect_materials(child, into);
    }
}

fn collect_geometry(node: &pcb::Node, geometry: &mut gpu::Geometry) {
    let min = node.bounds().min();
    let max = node.bounds().max();
    for axis in 0..3 {
        geometry.bounds.0[axis] = geometry.bounds.0[axis].min(min[axis]);
        geometry.bounds.1[axis] = geometry.bounds.1[axis].max(max[axis]);
    }

    for primitive in node.primitives() {
        let [a, b, c] = primitive.indices();
        let positions = [
            node.vertices()[usize::from(a)],
            node.vertices()[usize::from(b)],
            node.vertices()[usize::from(c)],
        ];
        let paint = color(primitive.material()).map(|channel| f32::from(channel) / 255.0);
        let normal = (Vec3::from_array(positions[1]) - Vec3::from_array(positions[0]))
            .cross(Vec3::from_array(positions[2]) - Vec3::from_array(positions[0]))
            .try_normalize()
            .unwrap_or(Vec3::Y)
            .to_array();
        let base = geometry.triangle_vertices.len() as u32;
        for position in positions {
            geometry.triangle_vertices.push(gpu::Vertex {
                position,
                normal,
                color: [paint[0], paint[1], paint[2], 1.0],
            });
        }
        geometry.triangle_indices.extend([base, base + 1, base + 2]);
    }

    add_box(geometry, min, max);

    for child in node.children() {
        collect_geometry(child, geometry);
    }
}

/// Whether an entry's mesh file has been asked for, and what came back.
enum EntryState {
    Wanted,
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Done,
    Failed,
}

/// Fetches a list's sub-meshes a few at a time and hands each one's geometry to the scene as it
/// decodes, so the view fills in around the camera rather than waiting for all of it.
struct Loader {
    files: Rc<dyn FileProvider>,
    dir: String,
    entries: Vec<pcb::MeshListEntry>,
    state: Vec<EntryState>,
}

impl Loader {
    fn new(files: Rc<dyn FileProvider>, path: &str, entries: Vec<pcb::MeshListEntry>) -> Self {
        let dir = path
            .rsplit_once('/')
            .map_or(String::new(), |(dir, _)| dir.to_owned());
        let state = entries.iter().map(|_| EntryState::Wanted).collect();
        Self {
            files,
            dir,
            entries,
            state,
        }
    }

    fn loaded(&self) -> usize {
        self.state
            .iter()
            .filter(|state| matches!(state, EntryState::Done | EntryState::Failed))
            .count()
    }

    fn mesh_path(&self, index: usize) -> String {
        let file = pcb::MeshList::mesh_file(self.entries[index].id());
        match self.dir.is_empty() {
            true => file,
            false => format!("{}/{file}", self.dir),
        }
    }

    fn poll(&mut self, ctx: &egui::Context, scene: &MeshScene, materials: &RefCell<Materials>) {
        let mut fetching = self
            .state
            .iter()
            .filter(|state| matches!(state, EntryState::Fetching(_)))
            .count();
        for index in 0..self.entries.len() {
            if fetching >= FETCHES {
                break;
            }
            if !matches!(self.state[index], EntryState::Wanted) {
                continue;
            }
            let files = self.files.clone();
            let path = self.mesh_path(index);
            self.state[index] = EntryState::Fetching(TrackedPromise::spawn_local(async move {
                files.read(&path).await
            }));
            fetching += 1;
        }

        for index in 0..self.entries.len() {
            let EntryState::Fetching(promise) = &self.state[index] else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            let path = self.mesh_path(index);
            self.state[index] = match result {
                Ok(bytes) => match decode_mesh(bytes.clone()) {
                    Ok((geometry, held)) => {
                        scene.queue(Arc::new(geometry));
                        let mut into = materials.borrow_mut();
                        for (word, count) in held {
                            *into.entry(word).or_default() += count;
                        }
                        EntryState::Done
                    }
                    Err(why) => {
                        log::error!("assets/pcb: {path}: {why}");
                        EntryState::Failed
                    }
                },
                Err(why) => {
                    log::error!("assets/pcb: {path}: {why}");
                    EntryState::Failed
                }
            };
        }

        if self.loaded() < self.entries.len() {
            ctx.request_repaint();
        }
    }
}

fn decode_mesh(bytes: Vec<u8>) -> Result<(gpu::Geometry, Materials)> {
    let mesh = match pcb::Collision::read(Cursor::new(bytes))? {
        pcb::Collision::Mesh(mesh) => mesh,
        pcb::Collision::List(_) => anyhow::bail!("列表不能引用另一个列表"),
    };
    let mut geometry = gpu::Geometry::new();
    collect_geometry(mesh.root(), &mut geometry);
    let mut materials = Materials::new();
    collect_materials(mesh.root(), &mut materials);
    Ok((geometry, materials))
}

impl MeshScene {
    fn new(bounds: ([f32; 3], [f32; 3]), meshes: usize) -> Self {
        let (min, max) = bounds;
        let (target, reach) = if min[0] <= max[0] {
            (
                Vec3::from_array([
                    (min[0] + max[0]) * 0.5,
                    (min[1] + max[1]) * 0.5,
                    (min[2] + max[2]) * 0.5,
                ]),
                Vec3::from_array([max[0] - min[0], max[1] - min[1], max[2] - min[2]])
                    .length()
                    .max(f32::EPSILON)
                    * 0.5,
            )
        } else {
            (Vec3::ZERO, 1.0)
        };
        let home = Camera {
            yaw: 0.7,
            pitch: 0.5,
            distance: reach * 1.8,
            target,
        };
        Self {
            renderer: gpu::Renderer::new(),
            camera: Cell::new(home),
            home,
            reach,
            triangles: Cell::new(0),
            meshes,
        }
    }

    fn queue(&self, geometry: Arc<gpu::Geometry>) {
        self.triangles
            .set(self.triangles.get() + geometry.triangle_indices.len() / 3);
        self.renderer.lock().unwrap().queue(geometry);
    }

    fn ui(&self, ui: &mut egui::Ui, show_wire: bool) {
        if let Some(failure) = self.renderer.lock().unwrap().failure() {
            ui.centered_and_justified(|ui| {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("无法绘制: {failure}"),
                );
            });
            return;
        }
        if self.triangles.get() == 0 && self.meshes == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("此网格没有三角形" ).weak());
            });
            return;
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("拖拽旋转，右键拖拽平移，滚轮缩放" ).weak());
            if ui.button("重置视图").clicked() {
                self.camera.set(self.home);
            }
        });
        ui.add_space(4.0);
        self.viewport(ui, show_wire);
    }

    fn viewport(&self, ui: &mut egui::Ui, show_wire: bool) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }

        let mut camera = self.camera.get();
        let pan = |camera: &mut Camera, delta: egui::Vec2| {
            let (sin_yaw, cos_yaw) = camera.yaw.sin_cos();
            let right = Vec3::new(cos_yaw, 0.0, -sin_yaw);
            let scale = camera.distance * 0.002;
            camera.target += (right * -delta.x + Vec3::Y * delta.y) * scale;
        };
        let zoom = |camera: &mut Camera, scale: f32| {
            camera.distance = (camera.distance * scale)
                .clamp(self.home.distance * 0.005, self.home.distance * 20.0);
        };

        let touch = ui.input(|input| input.multi_touch());
        match touch.filter(|_| response.dragged()) {
            Some(touch) => {
                zoom(&mut camera, 1.0 / touch.zoom_delta);
                pan(&mut camera, touch.translation_delta);
            }
            None => {
                if response.dragged_by(egui::PointerButton::Primary) {
                    let delta = response.drag_delta();
                    camera.yaw -= delta.x * 0.01;
                    camera.pitch = (camera.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
                }
                if response.dragged_by(egui::PointerButton::Secondary) {
                    pan(&mut camera, response.drag_delta());
                }
            }
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                zoom(&mut camera, 1.0 - scroll * 0.002);
            }
        }
        self.camera.set(camera);

        let eye = camera.eye();
        let view = glam::Mat4::look_at_rh(eye, camera.target, Vec3::Y);
        let span = (eye - self.home.target).length();
        let near = (span - self.reach).max(self.reach * 0.002);
        let projection = glam::Mat4::perspective_rh_gl(
            55.0_f32.to_radians(),
            rect.width() / rect.height(),
            near,
            span + self.reach,
        );

        let view_projection = (projection * view).to_cols_array();
        let renderer = self.renderer.clone();
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
                renderer.lock().unwrap().draw(
                    painter.gl(),
                    painter,
                    &view_projection,
                    &eye.to_array(),
                    show_wire,
                );
            })),
        });
    }
}

fn add_box(geometry: &mut gpu::Geometry, min: [f32; 3], max: [f32; 3]) {
    const COLOR: [f32; 4] = [0.9, 0.55, 0.25, 1.0];
    let corners = [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [max[0], max[1], max[2]],
        [min[0], max[1], max[2]],
    ];
    let base = geometry.wire_vertices.len() as u32;
    geometry
        .wire_vertices
        .extend(corners.into_iter().map(|position| gpu::Vertex {
            position,
            normal: [0.0, 1.0, 0.0],
            color: COLOR,
        }));
    geometry.wire_indices.extend([
        base,
        base + 1,
        base + 1,
        base + 2,
        base + 2,
        base + 3,
        base + 3,
        base,
        base + 4,
        base + 5,
        base + 5,
        base + 6,
        base + 6,
        base + 7,
        base + 7,
        base + 4,
        base,
        base + 4,
        base + 1,
        base + 5,
        base + 2,
        base + 6,
        base + 3,
        base + 7,
    ]);
}
