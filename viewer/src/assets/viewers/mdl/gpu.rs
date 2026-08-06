//! The GL side of the model viewer.
//!
//! Everything here runs inside an [`egui_glow`] paint callback, which is the only place a
//! `glow::Context` is reachable: the context is neither `Send` nor `Sync` on wasm, so it cannot be
//! captured, and eframe's copy of it is not threaded down to a viewer. Uploads therefore happen on
//! the first frame that draws rather than when the file is decoded, and freeing happens in a
//! graveyard the next callback drains.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use egui::TextureId;
use glow::HasContext;

use super::deferred::{self, Layered, Linked, TARGETS, TYPES, build_pair, dwords, sampler};
use super::material::Family;
use super::{Vertex, program};

pub use super::deferred::{Dead, LIT, Lighting, bury, graveyard};

/// Attribute locations, in the order [`Vertex`] stores them.
const ATTRIBUTES: [(u32, i32, i32); 4] = [(0, 3, 0), (1, 3, 12), (2, 4, 24), (3, 2, 56)];
const COLOR: u32 = 4;
const COLOR_OFFSET: i32 = 88;

/// Where each semantic a drawing package asks for sits in a [`Vertex`], and how wide it is. The
/// bytes are read as integers where the shader's own signature declares them so.
const FIELDS: [(program::Field, i32, i32, u32); 10] = [
    (program::Field::Position, 3, 0, glow::FLOAT),
    (program::Field::Normal, 3, 12, glow::FLOAT),
    (program::Field::Tangent, 4, 24, glow::FLOAT),
    (program::Field::Bitangent, 4, 40, glow::FLOAT),
    (program::Field::Uv, 4, 56, glow::FLOAT),
    (program::Field::Uv1, 4, 72, glow::FLOAT),
    (program::Field::Color, 4, 88, glow::UNSIGNED_BYTE),
    (program::Field::Color1, 4, 92, glow::UNSIGNED_BYTE),
    (program::Field::Weights, 4, 96, glow::UNSIGNED_SHORT),
    (program::Field::Bones, 4, 104, glow::UNSIGNED_SHORT),
];

/// The color table, which the game's own shaders address as a texture of their own.
const TABLE: u32 = 0x2005_679f;

/// Joints the palette a skinned shader reads is sized for.
const JOINTS: usize = 256;

/// Texture units, in the order the shader's samplers declare them.
const NORMAL_UNIT: u32 = 0;
const INDEX_UNIT: u32 = 1;
const MASK_UNIT: u32 = 2;
const DIFFUSE_UNIT: u32 = 3;
const TABLE_UNIT: u32 = 4;

/// Texels per color-table row. This viewer's own packing, not the game's.
pub const TABLE_COLUMNS: i32 = 4;

const VERTEX_SOURCE: &str = include_str!("model.vert");
const FRAGMENT_SOURCE: &str = include_str!("model.frag");

/// A mesh's geometry, once it is on the card.
struct Buffers {
    layout: glow::VertexArray,
    vertices: glow::Buffer,
    indices: glow::Buffer,
}

/// One material drawn with the shaders the game would draw it with.
pub struct Shaded {
    /// The buffer pass, one reading per page of its targets: a context promised four draw buffers
    /// fills a five-target G-buffer by running the pass more than once.
    pub buffer: Vec<Arc<program::Program>>,
    /// The depth pass, which runs first so the buffer pass shades nothing it covers.
    pub depth: Option<Arc<program::Program>>,
    /// What the material resolves itself into the frame with, drawn as its own geometry over what
    /// the lighting left. A semitransparent package has only this: it writes no G-buffer at all,
    /// and what it blends over is the frame the composite already resolved.
    pub resolve: Option<Arc<program::Program>>,
    /// The color table in the game's own layout: its halfs, the texels a row takes, and the rows.
    pub table: Option<Arc<(Vec<u16>, usize, usize)>>,
    /// The textures the material binds, by the resource id the package knows each by.
    pub textures: Vec<(u32, Option<TextureId>)>,
}

/// What one draw call needs beyond its geometry: the material it uses, and the egui textures that
/// material resolved to.
pub struct Surface {
    pub material: usize,
    pub shaded: Option<Shaded>,
    /// Which of the mesh's indices to draw, so a hidden part costs no triangles.
    pub runs: Vec<Range<i32>>,
    pub family: Family,
    pub normal: Option<TextureId>,
    pub index: Option<TextureId>,
    pub mask: Option<TextureId>,
    pub diffuse: Option<TextureId>,
    pub alpha_threshold: f32,
    pub diffuse_color: [f32; 3],
    pub emissive_color: [f32; 3],
    pub normal_scale: f32,
    pub cull: bool,
}

/// What a mesh draws as while its material is still being fetched: bare geometry, nothing tinted
/// away and nothing clipped.
impl Default for Surface {
    fn default() -> Self {
        Self {
            material: 0,
            shaded: None,
            runs: Vec::new(),
            family: Family::Background,
            normal: None,
            index: None,
            mask: None,
            diffuse: None,
            alpha_threshold: 0.0,
            diffuse_color: [1.0; 3],
            emissive_color: [0.0; 3],
            normal_scale: 1.0,
            cull: false,
        }
    }
}

/// What the shader draws instead of a shaded surface. Discriminants are the values `model.frag`
/// compares `u_debug` against.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Debug {
    None = 0,
    Normals = 1,
    Uv = 2,
    Geometry = 3,
    Tangents = 4,
    Bitangents = 5,
    Handedness = 6,
    Color = 7,
    Alpha = 8,
    Meshes = 9,
}

/// One frame of camera and material bindings, rebuilt every time the widget draws.
pub struct Frame {
    pub view: [f32; 16],
    pub projection: [f32; 16],
    /// Which G-buffer channel the game-shader path puts on screen, or the lit frame past the last
    /// of them.
    pub target: usize,
    /// What the game's own shaders are given that no file says: the camera in the convention they
    /// were compiled for, the frame's size, and the one light this viewer lights with.
    pub scene: program::Scene,
    /// The passes that light the G-buffer, once their packages have arrived.
    pub lighting: Option<Arc<Lighting>>,
    pub eye: [f32; 3],
    /// Key, fill and rim directions, in world space. Built once a frame from the camera, so a
    /// surface is lit by one set of lights rather than by a set of its own.
    pub lights: [f32; 9],
    pub surfaces: Vec<Surface>,
    pub debug: Debug,
}

/// Geometry waiting for a context to upload it under.
#[derive(Default)]
pub struct Pending {
    pub meshes: Vec<(Vec<Vertex>, Vec<u16>)>,
}

/// The card's side of drawing one model with the game's own shaders: the frame of the graph, and one
/// linked program per material.
#[derive(Default)]
struct Game {
    buffers: deferred::Buffers,
    joints: Option<glow::Texture>,
    programs: BTreeMap<(usize, bool, usize), Linked>,
    tables: BTreeMap<usize, glow::Texture>,
    /// The array these shaders bind their attributes into. An array holds the enable flags and the
    /// pointers, and a mesh's own array holds the layout the preview path was uploaded with, so
    /// laying a shader's own semantics over it would leave the preview reading the wrong fields.
    layout: Option<glow::VertexArray>,
    failure: Option<String>,
}

/// Everything the callback owns, shared with the viewer that built it.
pub struct Model {
    pending: Option<Pending>,
    program: Option<glow::Program>,
    game: Game,
    meshes: Vec<Buffers>,
    /// Color tables arrive with their materials, which is long after the geometry, so they queue
    /// rather than travelling with it.
    queued: Vec<(usize, Vec<f32>)>,
    /// Meshes whose indices a shape key rewrote, waiting for a context to upload them under.
    rewritten: Vec<(usize, Vec<u16>)>,
    /// The game's own layered textures, waiting for the same.
    arrays: Vec<(u32, Layered)>,
    tables: BTreeMap<usize, (glow::Texture, f32)>,
    /// Why the shader would not build, kept so the viewer can say so rather than draw nothing.
    failure: Option<String>,
}

impl Model {
    pub fn new(pending: Pending) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            pending: Some(pending),
            program: None,
            game: Game::default(),
            meshes: Vec::new(),
            queued: Vec::new(),
            rewritten: Vec::new(),
            arrays: Vec::new(),
            tables: BTreeMap::new(),
            failure: None,
        }))
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref().or(self.game.failure.as_deref())
    }

    /// How much of the G-buffer one pass can write. Four until a frame has asked the context, since
    /// that is what a context is promised.
    pub fn attachments(&self) -> usize {
        self.game.buffers.attachments()
    }

    /// Hands a material's color table over for the next draw to upload.
    pub fn queue_table(&mut self, material: usize, values: Vec<f32>) {
        self.queued.push((material, values));
    }

    /// Hands a mesh's indices over for the next draw to upload, replacing the ones it holds.
    pub fn queue_indices(&mut self, mesh: usize, indices: Vec<u16>) {
        self.rewritten.push((mesh, indices));
    }

    /// Hands one of the game's own layered textures over, under the resource id its shaders name it
    /// by.
    pub fn queue_array(&mut self, id: u32, held: Layered) {
        self.arrays.push((id, held));
    }

    pub fn draw(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        info: &egui::PaintCallbackInfo,
    ) {
        bury(gl);
        if self.failure.is_some() {
            return;
        }
        if let Some(pending) = self.pending.take()
            && let Err(why) = self.upload(gl, pending)
        {
            self.failure = Some(why);
            return;
        }
        for (mesh, indices) in std::mem::take(&mut self.rewritten) {
            let Some(buffers) = self.meshes.get(mesh) else {
                continue;
            };
            // Through the mesh's own vertex array, since binding an element buffer rewrites
            // whichever array is current, and egui leaves its own bound around a callback.
            unsafe {
                gl.bind_vertex_array(Some(buffers.layout));
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(buffers.indices));
                gl.buffer_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    bytemuck::cast_slice(&indices),
                    glow::STATIC_DRAW,
                );
                gl.bind_vertex_array(None);
            }
        }
        for (id, held) in std::mem::take(&mut self.arrays) {
            if let Err(why) = self.game.buffers.layered(gl, id, &held) {
                log::error!("assets/mdl: texture array {id:#010x}: {why}");
            }
        }
        for (material, values) in std::mem::take(&mut self.queued) {
            let rows = values.len() as i32 / (TABLE_COLUMNS * 4);
            match upload_table(gl, &values, rows) {
                Ok(texture) => {
                    self.tables.insert(material, (texture, rows as f32));
                }
                Err(why) => log::error!("assets/mdl: color table: {why}"),
            }
        }
        let Some(program) = self.program else {
            return;
        };
        // A zip would truncate instead, and a mesh drawn under another mesh's material shows as a
        // texturing bug rather than as the bookkeeping error it is.
        if self.meshes.len() != frame.surfaces.len() {
            self.failure = Some(format!(
                "{} meshes against {} surfaces",
                self.meshes.len(),
                frame.surfaces.len()
            ));
            return;
        }

        if frame.surfaces.iter().any(|held| held.shaded.is_some()) {
            self.game.draw(gl, painter, frame, &self.meshes, info);
            return;
        }

        unsafe {
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.disable(glow::BLEND);
            gl.use_program(Some(program));

            let view = gl.get_uniform_location(program, "u_view");
            gl.uniform_matrix_4_f32_slice(view.as_ref(), false, &frame.view);
            let projection = gl.get_uniform_location(program, "u_projection");
            gl.uniform_matrix_4_f32_slice(projection.as_ref(), false, &frame.projection);
            let eye = gl.get_uniform_location(program, "u_eye");
            gl.uniform_3_f32_slice(eye.as_ref(), &frame.eye);
            let lights = gl.get_uniform_location(program, "u_lights[0]");
            gl.uniform_3_f32_slice(lights.as_ref(), &frame.lights);
            for (name, unit) in [
                ("u_normal_map", NORMAL_UNIT),
                ("u_index_map", INDEX_UNIT),
                ("u_mask_map", MASK_UNIT),
                ("u_diffuse_map", DIFFUSE_UNIT),
                ("u_table", TABLE_UNIT),
            ] {
                let slot = gl.get_uniform_location(program, name);
                gl.uniform_1_i32(slot.as_ref(), unit as i32);
            }
            let debug = gl.get_uniform_location(program, "u_debug");
            gl.uniform_1_i32(debug.as_ref(), frame.debug as i32);
            let have = gl.get_uniform_location(program, "u_have");
            let family = gl.get_uniform_location(program, "u_family");
            let mesh = gl.get_uniform_location(program, "u_mesh");
            let threshold = gl.get_uniform_location(program, "u_alpha_threshold");
            let rows = gl.get_uniform_location(program, "u_table_rows");
            let diffuse = gl.get_uniform_location(program, "u_diffuse_color");
            let emissive = gl.get_uniform_location(program, "u_emissive_color");
            let scale = gl.get_uniform_location(program, "u_normal_scale");

            for (at, (buffers, surface)) in self.meshes.iter().zip(&frame.surfaces).enumerate() {
                if surface.runs.is_empty() {
                    continue;
                }
                match surface.cull {
                    true => {
                        gl.enable(glow::CULL_FACE);
                        gl.cull_face(glow::BACK);
                        gl.front_face(glow::CCW);
                    }
                    false => gl.disable(glow::CULL_FACE),
                }

                let table = self.tables.get(&surface.material).copied();
                let mut bound = 0;
                for (unit, id) in [
                    (NORMAL_UNIT, surface.normal),
                    (INDEX_UNIT, surface.index),
                    (MASK_UNIT, surface.mask),
                    (DIFFUSE_UNIT, surface.diffuse),
                ] {
                    let texture = id.and_then(|id| painter.texture(id));
                    gl.active_texture(glow::TEXTURE0 + unit);
                    gl.bind_texture(glow::TEXTURE_2D, texture);
                    bound |= i32::from(texture.is_some()) << unit;
                }
                gl.active_texture(glow::TEXTURE0 + TABLE_UNIT);
                gl.bind_texture(glow::TEXTURE_2D, table.map(|(texture, _)| texture));
                bound |= i32::from(table.is_some()) << TABLE_UNIT;

                gl.uniform_1_i32(have.as_ref(), bound);
                gl.uniform_1_i32(family.as_ref(), surface.family as i32);
                gl.uniform_1_i32(mesh.as_ref(), at as i32);
                gl.uniform_1_f32(threshold.as_ref(), surface.alpha_threshold);
                gl.uniform_1_f32(rows.as_ref(), table.map_or(0.0, |(_, rows)| rows));
                gl.uniform_3_f32_slice(diffuse.as_ref(), &surface.diffuse_color);
                gl.uniform_3_f32_slice(emissive.as_ref(), &surface.emissive_color);
                gl.uniform_1_f32(scale.as_ref(), surface.normal_scale);

                gl.bind_vertex_array(Some(buffers.layout));
                for run in &surface.runs {
                    let offset = run.start * size_of::<u16>() as i32;
                    gl.draw_elements(
                        glow::TRIANGLES,
                        run.end - run.start,
                        glow::UNSIGNED_SHORT,
                        offset,
                    );
                }
            }

            gl.bind_vertex_array(None);
            gl.depth_mask(false);
        }
    }

    fn upload(&mut self, gl: &glow::Context, pending: Pending) -> Result<(), String> {
        // `antialias` on the canvas is a hint the implementation may ignore, and nothing short of a
        // live context says whether it did. DEPTH_BITS is not asked alongside it: a core profile
        // dropped that query, and asking raises an error the frame is then blamed for.
        let samples = unsafe { gl.get_parameter_i32(glow::SAMPLES) };
        log::info!(
            "assets/mdl: {} meshes on {:?}, {samples} samples",
            pending.meshes.len(),
            gl.version()
        );
        self.program = Some(build(gl)?);
        for (vertices, indices) in &pending.meshes {
            self.meshes.push(upload_mesh(gl, vertices, indices)?);
        }
        Ok(())
    }
}

impl Game {
    /// The joint palette, which every skinned shader reads through a texture of dwords. Rewritten
    /// each frame, since a joint carries the camera as well as the pose.
    fn palette(&mut self, gl: &glow::Context, transform: glam::Mat4) -> Result<(), String> {
        let held = dwords(gl, &program::joints(JOINTS, transform))?;
        if let Some(stale) = self.joints.replace(held) {
            graveyard().lock().unwrap().push(Dead::Texture(stale));
        }
        Ok(())
    }

    fn layout(&mut self, gl: &glow::Context) -> Result<glow::VertexArray, String> {
        if let Some(held) = self.layout {
            return Ok(held);
        }
        let held = unsafe { gl.create_vertex_array()? };
        self.layout = Some(held);
        Ok(held)
    }

    /// The material's color table, in the layout its own shaders address it in.
    fn table(
        &mut self,
        gl: &glow::Context,
        material: usize,
        held: &(Vec<u16>, usize, usize),
    ) -> Result<glow::Texture, String> {
        if let Some(texture) = self.tables.get(&material) {
            return Ok(*texture);
        }
        let (values, columns, rows) = held;
        unsafe {
            let texture = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA16F as i32,
                *columns as i32,
                *rows as i32,
                0,
                glow::RGBA,
                glow::HALF_FLOAT,
                glow::PixelUnpackData::Slice(Some(bytemuck::cast_slice(values))),
            );
            // Filtered, because the shader addresses a row pair by landing between the two of them
            // and leaves the mix to the sampler. Every other read it makes is of a texel center,
            // which filtering answers exactly.
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
            }
            self.tables.insert(material, texture);
            Ok(texture)
        }
    }

    fn draw(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        meshes: &[Buffers],
        info: &egui::PaintCallbackInfo,
    ) {
        let held = info.viewport_in_pixels();
        let size = (held.width_px.max(1), held.height_px.max(1));
        // egui draws into whatever it bound before the callback, and that has to be bound again
        // whether or not the frame drew. Asking the painter rather than the context is what makes
        // this work on the web: glow keeps its own map of the resources it created, and a
        // framebuffer read back out of WebGL is a JS object it cannot find in there.
        let bound = painter.intermediate_fbo();
        let drawn = self.render(gl, painter, frame, meshes, size);
        let shown = self.buffers.show(
            gl,
            frame.target,
            bound,
            (held.left_px, held.from_bottom_px, size.0, size.1),
        );
        self.failure = drawn.and(shown).err();
    }

    fn render(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        meshes: &[Buffers],
        size: (i32, i32),
    ) -> Result<(), String> {
        self.buffers.attach(gl, size)?;
        self.buffers.stand_ins(gl)?;
        // Only the callback knows how many pixels the widget really covers, and a screen-wide pass
        // has nothing else to turn a fragment into a texel with.
        let scene = program::Scene {
            size: (size.0 as f32, size.1 as f32),
            ..frame.scene
        };
        self.palette(gl, scene.view * scene.model)?;

        // The G-buffer a page at a time, and each page's depth pass before its buffer pass: the game
        // runs those as two passes over the whole draw rather than as two draws of one surface.
        for page in 0..self.buffers.pages() {
            self.buffers.open(gl, page);
            for depth in [true, false] {
                for (buffers, surface) in meshes.iter().zip(&frame.surfaces) {
                    let Some(shaded) = &surface.shaded else {
                        continue;
                    };
                    if surface.runs.is_empty() {
                        continue;
                    }
                    let held = match depth {
                        true => shaded.depth.as_deref(),
                        false => shaded.buffer.get(page).map(Arc::as_ref),
                    };
                    let Some(held) = held.filter(|held| depth || !held.targets.is_empty()) else {
                        continue;
                    };
                    let program = deferred::link(
                        gl,
                        &mut self.programs,
                        (surface.material, depth, page),
                        held,
                    )?;
                    unsafe {
                        gl.use_program(Some(program));
                        // A material with no depth pass writes its own, since the depth buffer is
                        // what says which pixels the frame covered.
                        gl.depth_mask(depth || shaded.depth.is_none());
                        gl.color_mask(!depth, !depth, !depth, !depth);
                        let written: Vec<u32> = (0..held.targets.len().max(1))
                            .map(|at| glow::COLOR_ATTACHMENT0 + at as u32)
                            .collect();
                        gl.draw_buffers(&written);
                        match surface.cull {
                            true => {
                                gl.enable(glow::CULL_FACE);
                                gl.cull_face(glow::BACK);
                                gl.front_face(glow::CCW);
                            }
                            false => gl.disable(glow::CULL_FACE),
                        }
                    }
                    self.bind(gl, painter, program, held, surface, buffers, &scene)?;
                }
            }
        }

        // Only where the lit frame is what is being shown: a raw channel is a page of the G-buffer
        // and owes nothing to the passes past it.
        if let Some(lighting) = frame.lighting.as_ref().filter(|_| frame.target >= TARGETS) {
            self.buffers
                .resolve(gl, lighting, &scene, &[frame.scene.lamp])?;
            self.resolve(gl, painter, frame, meshes, &scene)?;
        }
        Ok(())
    }

    /// Every material resolved into the frame as its own geometry, after the lighting.
    ///
    /// A material that wrote the G-buffer goes first and reads it; one that did not wrote nothing
    /// to light and reads the frame instead, so the copy it reads is taken once the rest have
    /// drawn. Depth tested against what the G-buffer covered and writing none of its own, so the
    /// surfaces in front of a piece of glass hide it and the pieces behind it do not.
    fn resolve(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        meshes: &[Buffers],
        scene: &program::Scene,
    ) -> Result<(), String> {
        let (opaque, blended): (Vec<usize>, Vec<usize>) = frame
            .surfaces
            .iter()
            .enumerate()
            .filter(|(_, surface)| {
                surface
                    .shaded
                    .as_ref()
                    .is_some_and(|shaded| shaded.resolve.is_some())
                    && !surface.runs.is_empty()
            })
            .map(|(at, _)| at)
            .partition(|at| {
                frame.surfaces[*at]
                    .shaded
                    .as_ref()
                    .is_some_and(|shaded| !shaded.buffer.is_empty())
            });
        for (behind, held) in [(false, &opaque), (true, &blended)] {
            if held.is_empty() {
                continue;
            }
            if behind {
                self.buffers.keep(gl)?;
            }
            let into = self.buffers.frame().ok_or("no lit frame")?;
            let size = self.buffers.size();
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
                gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
                gl.viewport(0, 0, size.0, size.1);
                gl.color_mask(true, true, true, true);
                gl.enable(glow::DEPTH_TEST);
                gl.depth_func(glow::LEQUAL);
                gl.depth_mask(false);
                gl.disable(glow::BLEND);
            }
            for at in held {
                let surface = &frame.surfaces[*at];
                let Some(mesh) = meshes.get(*at) else {
                    continue;
                };
                let held = surface
                    .shaded
                    .as_ref()
                    .and_then(|shaded| shaded.resolve.as_deref())
                    .ok_or("no pass to resolve with")?;
                let program =
                    deferred::link(gl, &mut self.programs, (surface.material, false, LIT), held)?;
                unsafe {
                    gl.use_program(Some(program));
                    match surface.cull {
                        true => {
                            gl.enable(glow::CULL_FACE);
                            gl.cull_face(glow::BACK);
                            gl.front_face(glow::CCW);
                        }
                        false => gl.disable(glow::CULL_FACE),
                    }
                }
                self.bind(gl, painter, program, held, surface, mesh, scene)?;
            }
        }
        Ok(())
    }

    /// What one draw of one material binds, and the geometry it covers. A texture the material has
    /// nothing for is the frame's own where the graph holds one under that name, and the flat
    /// stand-in otherwise.
    #[allow(clippy::too_many_arguments)]
    fn bind(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        program: glow::Program,
        held: &program::Program,
        surface: &Surface,
        mesh: &Buffers,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let shaded = surface.shaded.as_ref().ok_or("nothing to draw with")?;
        let palette = self.joints.ok_or("no joint palette")?;
        let layout = self.layout(gl)?;
        let size = self.buffers.size();
        self.buffers.bind(gl, program, held, scene, &[])?;
        // Before anything is bound: making a texture binds it to whichever unit happens to be
        // active, so one made partway through the loop below takes over the unit the sampler
        // before it was just given.
        let table = match &shaded.table {
            Some(table) => Some(self.table(gl, surface.material, table)?),
            None => None,
        };
        let mut unit = 0;
        for texture in &held.textures {
            // Only a plane can come from the material: what it binds is an egui texture, and egui
            // has nothing but two-dimensional ones.
            let bound = match texture.kind {
                program::Kind::Plane => {
                    let held = match texture.id {
                        TABLE => table,
                        id => shaded
                            .textures
                            .iter()
                            .find(|(held, _)| *held == id)
                            .and_then(|(_, held)| *held)
                            .and_then(|held| painter.texture(held)),
                    };
                    match held {
                        Some(held) => held,
                        None => self.buffers.engine(gl, texture.id)?,
                    }
                }
                kind => self.buffers.absent(gl, kind, texture.id)?,
            };
            deferred::bind(
                gl,
                program,
                &texture.name,
                unit,
                bound,
                deferred::target(texture.kind),
            );
            unit += 1;
        }
        // By name, not by position: a character's buffer pass reads the joint palette and the
        // shader-type table both, and they hold different things.
        for structured in &held.structured {
            let bound = match structured.name.as_str() {
                TYPES => self.buffers.types(gl)?,
                _ => palette,
            };
            sampler(gl, program, &structured.name, unit, bound);
            unit += 1;
        }
        unsafe {
            if let Some(location) = gl.get_uniform_location(program, "dx_Viewport") {
                gl.uniform_2_f32(Some(&location), size.0 as f32, size.1 as f32);
            }
            gl.bind_vertex_array(Some(layout));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh.vertices));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(mesh.indices));
            for location in 0..16 {
                gl.disable_vertex_attrib_array(location);
            }
            for held in &held.attributes {
                attribute(gl, held);
            }
            for run in &surface.runs {
                gl.draw_elements(
                    glow::TRIANGLES,
                    run.end - run.start,
                    glow::UNSIGNED_SHORT,
                    run.start * size_of::<u16>() as i32,
                );
            }
            gl.bind_vertex_array(None);
        }
        Ok(())
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.tables.values().copied().map(Dead::Texture));
        dead.extend(self.joints.take().map(Dead::Texture));
        dead.extend(self.layout.take().map(Dead::Layout));
        dead.extend(
            std::mem::take(&mut self.programs)
                .into_values()
                .map(|held| Dead::Program(held.program)),
        );
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        graveyard().lock().unwrap().extend(
            self.meshes
                .drain(..)
                .flat_map(|held| {
                    [
                        Dead::Layout(held.layout),
                        Dead::Buffer(held.vertices),
                        Dead::Buffer(held.indices),
                    ]
                })
                .chain(
                    std::mem::take(&mut self.tables)
                        .into_values()
                        .map(|(texture, _)| Dead::Texture(texture)),
                )
                .chain(self.program.take().map(Dead::Program)),
        );
    }
}

/// Points one attribute at the field of a [`Vertex`] its semantic names. The mesh keeps its
/// influences unsigned and a shader declares them either way, so the pointer's own type follows the
/// signature: a draw is rejected outright where the two differ in class or in sign.
pub fn attribute(gl: &glow::Context, held: &program::Attribute) {
    let Some((_, lanes, offset, kind)) = FIELDS.iter().find(|(field, ..)| *field == held.field)
    else {
        return;
    };
    let stride = size_of::<Vertex>() as i32;
    unsafe {
        gl.enable_vertex_attrib_array(held.location);
        match held.components {
            program::Components::Float => gl.vertex_attrib_pointer_f32(
                held.location,
                *lanes,
                *kind,
                *kind == glow::UNSIGNED_BYTE,
                stride,
                *offset,
            ),
            program::Components::Unsigned => {
                gl.vertex_attrib_pointer_i32(held.location, *lanes, *kind, stride, *offset)
            }
            program::Components::Signed => gl.vertex_attrib_pointer_i32(
                held.location,
                *lanes,
                match *kind {
                    glow::UNSIGNED_BYTE => glow::BYTE,
                    _ => glow::SHORT,
                },
                stride,
                *offset,
            ),
        }
    }
}

/// One mesh's buffers, with the attribute layout captured in a vertex array of its own.
///
/// The array is not an optimisation. egui leaves its own vertex array bound while a callback runs,
/// so setting attribute pointers without one would rewrite egui's layout to point at model
/// geometry, and every widget drawn afterwards would read vertices out of this mesh.
fn upload_mesh(
    gl: &glow::Context,
    vertices: &[Vertex],
    indices: &[u16],
) -> Result<Buffers, String> {
    unsafe {
        let layout = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(layout));

        let held = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(held));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(vertices),
            glow::STATIC_DRAW,
        );

        let stride = size_of::<Vertex>() as i32;
        for (location, size, offset) in ATTRIBUTES {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_f32(location, size, glow::FLOAT, false, stride, offset);
        }
        gl.enable_vertex_attrib_array(COLOR);
        gl.vertex_attrib_pointer_f32(COLOR, 4, glow::UNSIGNED_BYTE, true, stride, COLOR_OFFSET);

        let drawn = gl.create_buffer()?;
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(drawn));
        gl.buffer_data_u8_slice(
            glow::ELEMENT_ARRAY_BUFFER,
            bytemuck::cast_slice(indices),
            glow::STATIC_DRAW,
        );

        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        Ok(Buffers {
            layout,
            vertices: held,
            indices: drawn,
        })
    }
}

/// The color table, one RGBA texel per field group. Point sampled: the row pair is mixed in the
/// shader rather than by the sampler, so a row's own values stay exact.
fn upload_table(gl: &glow::Context, values: &[f32], rows: i32) -> Result<glow::Texture, String> {
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA16F as i32,
            TABLE_COLUMNS,
            rows,
            0,
            glow::RGBA,
            glow::FLOAT,
            glow::PixelUnpackData::Slice(Some(bytemuck::cast_slice(values))),
        );
        for (name, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
            (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
        }
        Ok(texture)
    }
}

fn build(gl: &glow::Context) -> Result<glow::Program, String> {
    build_pair(gl, VERTEX_SOURCE, FRAGMENT_SOURCE)
}
