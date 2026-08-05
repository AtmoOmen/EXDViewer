//! The GL side of the model viewer.
//!
//! Everything here runs inside an [`egui_glow`] paint callback, which is the only place a
//! `glow::Context` is reachable: the context is neither `Send` nor `Sync` on wasm, so it cannot be
//! captured, and eframe's copy of it is not threaded down to a viewer. Uploads therefore happen on
//! the first frame that draws rather than when the file is decoded, and freeing happens in a
//! graveyard the next callback drains.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use egui::TextureId;
use glow::HasContext;

use super::material::Family;
use super::{Vertex, program};

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
    (program::Field::Weights, 4, 96, glow::UNSIGNED_BYTE),
    (program::Field::Bones, 4, 100, glow::UNSIGNED_BYTE),
];

/// The color table, which the game's own shaders address as a texture of their own.
const TABLE: u32 = 0x2005_679f;

/// The frame's own buffers, as the lighting and composite passes name them.
const GBUFFER: [u32; 5] = [
    0xebbb_29bd,
    0xe4e5_7422,
    0x7dec_2598,
    0x0aeb_150e,
    0x948f_80ad,
];
const DEPTH: u32 = 0x2c8f_f4b0;
const VIEW_POSITION: u32 = 0xbc61_5663;
const LIGHT_DIFFUSE: u32 = 0x23d0_f850;
const LIGHT_SPECULAR: u32 = 0x6c19_aca4;
const OCCLUSION: u32 = 0x3266_7bd7;
const ATTENUATION: u32 = 0x008c_d1ca;

/// Joints the palette a skinned shader reads is sized for.
const JOINTS: usize = 256;

/// What a texture the material binds nothing to answers with.
const STAND_IN: [u8; 4] = [128, 128, 128, 255];

/// What a buffer nothing here fills answers with where a lighting pass wants a weight: nothing
/// shadowed in the red the lighting reads, nothing faded in the alpha the composite reads.
const UNOCCLUDED: [u8; 4] = [255, 255, 255, 0];

/// One triangle covering clip space, which is the geometry a screen-wide pass draws: their vertex
/// shaders pass the position straight through, and one of them reads it back as the place on screen
/// the pixel came from.
const SCREEN: [f32; 12] = [
    -1.0, -1.0, 0.0, 1.0, //
    3.0, -1.0, 0.0, 1.0, //
    -1.0, 3.0, 0.0, 1.0,
];

/// The corners and triangles of the volume a point light covers, which its own vertex shader clamps
/// to the light's extents and then projects. Not a screen-wide pass: a point light only reaches the
/// pixels its volume covers.
const VOLUME: [f32; 32] = [
    -1.0, -1.0, -1.0, 1.0, //
    1.0, -1.0, -1.0, 1.0, //
    1.0, 1.0, -1.0, 1.0, //
    -1.0, 1.0, -1.0, 1.0, //
    -1.0, -1.0, 1.0, 1.0, //
    1.0, -1.0, 1.0, 1.0, //
    1.0, 1.0, 1.0, 1.0, //
    -1.0, 1.0, 1.0, 1.0,
];
const VOLUME_FACES: [u16; 36] = [
    0, 2, 1, 0, 3, 2, // back
    4, 5, 6, 4, 6, 7, // front
    0, 1, 5, 0, 5, 4, // bottom
    3, 7, 6, 3, 6, 2, // top
    0, 4, 7, 0, 7, 3, // left
    1, 2, 6, 1, 6, 5, // right
];

/// Channels of the G-buffer, which is what its pages add up to however many a context can write at
/// once, and the channel past the last of them: the frame the composite resolved.
const TARGETS: usize = 5;
pub const LIT: usize = TARGETS;

/// The one structured buffer that is not a joint palette.
const TYPES: &str = "g_ShaderTypeParameter";

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

/// GL objects with nothing left to draw them, waiting for a context to delete them under. A viewer
/// is dropped between frames, where there is no context, so its objects outlive it by one callback.
static GRAVEYARD: OnceLock<Mutex<Vec<Dead>>> = OnceLock::new();

pub enum Dead {
    Layout(glow::VertexArray),
    Buffer(glow::Buffer),
    Texture(glow::Texture),
    Program(glow::Program),
    Renderbuffer(glow::Renderbuffer),
    Frame(glow::Framebuffer),
}

pub fn graveyard() -> &'static Mutex<Vec<Dead>> {
    GRAVEYARD.get_or_init(Default::default)
}

/// Deletes what an earlier viewer left behind. Called at the top of a draw, because that is the
/// only moment a context exists.
pub fn bury(gl: &glow::Context) {
    for dead in graveyard().lock().unwrap().drain(..) {
        unsafe {
            match dead {
                Dead::Layout(layout) => gl.delete_vertex_array(layout),
                Dead::Buffer(buffer) => gl.delete_buffer(buffer),
                Dead::Texture(texture) => gl.delete_texture(texture),
                Dead::Program(program) => gl.delete_program(program),
                Dead::Renderbuffer(held) => gl.delete_renderbuffer(held),
                Dead::Frame(held) => gl.delete_framebuffer(held),
            }
        }
    }
}

/// The passes that light what the G-buffer holds and resolve it into a frame, translated out of the
/// packages that hold them rather than out of the one a material names.
///
/// Each reads what the one before it wrote: the view position comes off the depth buffer, the light
/// off the view position and the G-buffer, and the frame off both.
pub struct Lighting {
    pub position: Arc<program::Program>,
    pub directional: Arc<program::Program>,
    pub point: Arc<program::Program>,
    pub composite: Arc<program::Program>,
}

/// One material drawn with the shaders the game would draw it with.
pub struct Shaded {
    /// The buffer pass, one reading per page of its targets: a context promised four draw buffers
    /// fills a five-target G-buffer by running the pass more than once.
    pub buffer: Vec<Arc<program::Program>>,
    /// The depth pass, which runs first so the buffer pass shades nothing it covers.
    pub depth: Option<Arc<program::Program>>,
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

/// A linked pair of the game's own shaders, and the source it was built from so a change rebuilds
/// it rather than a stale program drawing on.
struct Linked {
    source: String,
    program: glow::Program,
}

/// The card's side of drawing with the game's own shaders: a G-buffer written a page of targets at a
/// time, the buffers the lighting passes fill from it, and one linked program per material.
#[derive(Default)]
struct Game {
    /// One framebuffer per page of the G-buffer's targets, all sharing the depth texture.
    frames: Vec<glow::Framebuffer>,
    color: Vec<glow::Texture>,
    /// A texture rather than a renderbuffer: the lighting passes read the depth back.
    depth: Option<glow::Texture>,
    /// The view position, the light this frame accumulates, and the frame the composite resolves.
    position: Option<(glow::Framebuffer, glow::Texture)>,
    light: Option<(glow::Framebuffer, [glow::Texture; 2])>,
    lit: Option<(glow::Framebuffer, glow::Texture)>,
    size: (i32, i32),
    /// What the context allows, which is what decides how much of the G-buffer one pass can write.
    attachments: usize,
    joints: Option<glow::Texture>,
    types: Option<glow::Texture>,
    stand_in: Option<glow::Texture>,
    unoccluded: Option<glow::Texture>,
    screen: Option<(glow::VertexArray, glow::Buffer)>,
    volume: Option<(glow::VertexArray, glow::Buffer, glow::Buffer)>,
    programs: BTreeMap<(usize, bool, usize), Linked>,
    deferred: BTreeMap<usize, Linked>,
    tables: BTreeMap<usize, glow::Texture>,
    blocks: Vec<glow::Buffer>,
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
        match self.game.attachments {
            0 => 4,
            held => held,
        }
    }

    /// Hands a material's color table over for the next draw to upload.
    pub fn queue_table(&mut self, material: usize, values: Vec<f32>) {
        self.queued.push((material, values));
    }

    /// Hands a mesh's indices over for the next draw to upload, replacing the ones it holds.
    pub fn queue_indices(&mut self, mesh: usize, indices: Vec<u16>) {
        self.rewritten.push((mesh, indices));
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
        // live context says whether it did.
        let samples = unsafe { gl.get_parameter_i32(glow::SAMPLES) };
        let depth = unsafe { gl.get_parameter_i32(glow::DEPTH_BITS) };
        log::info!(
            "assets/mdl: {} meshes on {:?}, {samples} samples, {depth} depth bits",
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
    /// Every buffer of the graph, sized to what is being drawn into.
    ///
    /// The G-buffer is one framebuffer per page of its targets: a context is promised four draw
    /// buffers and a framebuffer no more color attachments than that, so five targets cannot all
    /// hang off one, and what a page cannot hold is written by a reading of its own.
    fn attach(&mut self, gl: &glow::Context, size: (i32, i32)) -> Result<(), String> {
        if self.attachments == 0 {
            let limit = unsafe { gl.get_parameter_i32(glow::MAX_DRAW_BUFFERS) };
            self.attachments = (limit.max(1) as usize).min(TARGETS);
        }
        if !self.frames.is_empty() && self.size == size {
            return Ok(());
        }
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.color.drain(..).map(Dead::Texture));
        dead.extend(self.depth.take().map(Dead::Texture));
        dead.extend(self.frames.drain(..).map(Dead::Frame));
        for (frame, textures) in [
            self.position
                .take()
                .map(|(frame, held)| (frame, vec![held])),
            self.light
                .take()
                .map(|(frame, held)| (frame, held.to_vec())),
            self.lit.take().map(|(frame, held)| (frame, vec![held])),
        ]
        .into_iter()
        .flatten()
        {
            dead.push(Dead::Frame(frame));
            dead.extend(textures.into_iter().map(Dead::Texture));
        }
        drop(dead);
        self.size = size;

        unsafe {
            let depth = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(depth));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::DEPTH_COMPONENT24 as i32,
                size.0,
                size.1,
                0,
                glow::DEPTH_COMPONENT,
                glow::UNSIGNED_INT,
                glow::PixelUnpackData::Slice(None),
            );
            point(gl);
            self.depth = Some(depth);

            for page in 0..TARGETS.div_ceil(self.attachments) {
                let held = gl.create_framebuffer()?;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(held));
                let width = (TARGETS - page * self.attachments).min(self.attachments);
                for at in 0..width {
                    // The channels the game stores as bytes, which is what makes the shader type in
                    // the first target's alpha come back the whole number it went in as.
                    let texture = plane(gl, size, glow::RGBA8, glow::RGBA, glow::UNSIGNED_BYTE)?;
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0 + at as u32,
                        glow::TEXTURE_2D,
                        Some(texture),
                        0,
                    );
                    self.color.push(texture);
                }
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_ATTACHMENT,
                    glow::TEXTURE_2D,
                    Some(depth),
                    0,
                );
                let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
                if status != glow::FRAMEBUFFER_COMPLETE {
                    return Err(format!("the G-buffer would not complete: {status:#x}"));
                }
                self.frames.push(held);
            }

            // The view position and the light the frame gathers hold values a byte cannot: one is a
            // place in front of the camera, the other a sum of every light that reached the pixel.
            let position = plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?;
            self.position = Some((frame_of(gl, &[position])?, position));
            let light = [
                plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?,
                plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?,
            ];
            self.light = Some((frame_of(gl, &light)?, light));
            // The composite answers with a color already brought into the range a screen shows, so
            // the frame it lands in is the one that can be blitted to the screen.
            let lit = plane(gl, size, glow::RGBA8, glow::RGBA, glow::UNSIGNED_BYTE)?;
            self.lit = Some((frame_of(gl, &[lit])?, lit));
        }
        Ok(())
    }

    /// The linked pair for one material's pass, built again whenever the translation changes.
    fn link(
        &mut self,
        gl: &glow::Context,
        key: (usize, bool, usize),
        held: &program::Program,
    ) -> Result<glow::Program, String> {
        let source = format!("{}\n{}", held.vertex, held.fragment);
        if let Some(linked) = self.programs.get(&key)
            && linked.source == source
        {
            return Ok(linked.program);
        }
        let built = build_pair(gl, &held.vertex, &held.fragment)?;
        if let Some(stale) = self.programs.insert(
            key,
            Linked {
                source,
                program: built,
            },
        ) {
            graveyard()
                .lock()
                .unwrap()
                .push(Dead::Program(stale.program));
        }
        Ok(built)
    }

    /// The one texture standing in for whatever a material binds nothing to.
    fn stand_in(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.stand_in {
            return Ok(held);
        }
        let held = flat(gl, &STAND_IN)?;
        self.stand_in = Some(held);
        Ok(held)
    }

    /// What a lighting pass reads where nothing occluded the pixel. Nothing here computes occlusion,
    /// so every pixel answers the same, and it is not the value a color map would stand in with.
    fn unoccluded(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.unoccluded {
            return Ok(held);
        }
        let held = flat(gl, &UNOCCLUDED)?;
        self.unoccluded = Some(held);
        Ok(held)
    }

    /// The table `SV_Target.w` indexes, which every pixel shader that shades a surface reads.
    fn types(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.types {
            return Ok(held);
        }
        let held = dwords(gl, &program::shader_types())?;
        self.types = Some(held);
        Ok(held)
    }

    /// The joint palette, which every skinned shader reads through a texture of dwords. Rewritten
    /// each frame, since a joint carries the camera as well as the pose.
    fn palette(
        &mut self,
        gl: &glow::Context,
        transform: glam::Mat4,
    ) -> Result<glow::Texture, String> {
        let held = dwords(gl, &program::joints(JOINTS, transform))?;
        if let Some(stale) = self.joints.replace(held) {
            graveyard().lock().unwrap().push(Dead::Texture(stale));
        }
        Ok(held)
    }

    /// The buffers a screen-wide pass reads, by the name the package knows each by. One nothing here
    /// fills is the flat stand-in, which is what leaves its term out rather than crashing the draw.
    fn engine(&mut self, gl: &glow::Context, id: u32) -> Result<glow::Texture, String> {
        if let Some(at) = GBUFFER.iter().position(|held| *held == id) {
            return self
                .color
                .get(at)
                .copied()
                .ok_or_else(|| format!("the G-buffer has no channel {at}"));
        }
        Ok(match id {
            DEPTH => self.depth.ok_or("no depth buffer")?,
            VIEW_POSITION => self.position.ok_or("no view position")?.1,
            LIGHT_DIFFUSE => self.light.ok_or("no light buffer")?.1[0],
            LIGHT_SPECULAR => self.light.ok_or("no light buffer")?.1[1],
            OCCLUSION | ATTENUATION => self.unoccluded(gl)?,
            _ => self.stand_in(gl)?,
        })
    }

    /// The uniform blocks one draw reads, filled and bound.
    fn bind(
        &mut self,
        gl: &glow::Context,
        program: glow::Program,
        held: &program::Program,
        scene: &program::Scene,
    ) -> Result<(), String> {
        for (at, buffer) in held.buffers.iter().enumerate() {
            let Some(block) =
                (unsafe { gl.get_uniform_block_index(program, &format!("{}_b", buffer.name)) })
            else {
                continue;
            };
            unsafe {
                let size = gl.get_active_uniform_block_parameter_i32(
                    program,
                    block,
                    glow::UNIFORM_BLOCK_DATA_SIZE,
                ) as usize;
                let mut data = buffer.fill(scene, held.pass);
                data.resize(size.max(16), 0);
                while self.blocks.len() <= at {
                    self.blocks.push(gl.create_buffer()?);
                }
                let held = self.blocks[at];
                gl.bind_buffer(glow::UNIFORM_BUFFER, Some(held));
                gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, &data, glow::DYNAMIC_DRAW);
                gl.bind_buffer_base(glow::UNIFORM_BUFFER, at as u32, Some(held));
                gl.uniform_block_binding(program, block, at as u32);
            }
        }
        Ok(())
    }

    /// One pass of the graph drawn over a framebuffer of its own, reading whatever the passes before
    /// it left behind. `volume` says whether it covers the frame or only what a light reaches.
    fn pass(
        &mut self,
        gl: &glow::Context,
        at: usize,
        held: &program::Program,
        into: glow::Framebuffer,
        scene: &program::Scene,
        volume: bool,
    ) -> Result<(), String> {
        let source = format!("{}\n{}", held.vertex, held.fragment);
        let program = match self.deferred.get(&at) {
            Some(linked) if linked.source == source => linked.program,
            _ => {
                let built = build_pair(gl, &held.vertex, &held.fragment)?;
                if let Some(stale) = self.deferred.insert(
                    at,
                    Linked {
                        source,
                        program: built,
                    },
                ) {
                    graveyard()
                        .lock()
                        .unwrap()
                        .push(Dead::Program(stale.program));
                }
                built
            }
        };
        let layout = match volume {
            true => {
                let held = match self.volume {
                    Some(held) => held,
                    None => {
                        let held = upload_volume(gl)?;
                        self.volume = Some(held);
                        held
                    }
                };
                held.0
            }
            false => {
                let held = match self.screen {
                    Some(held) => held,
                    None => {
                        let held = upload_screen(gl)?;
                        self.screen = Some(held);
                        held
                    }
                };
                held.0
            }
        };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            let written: Vec<u32> = (0..held.targets.len().max(1))
                .map(|slot| glow::COLOR_ATTACHMENT0 + slot as u32)
                .collect();
            gl.draw_buffers(&written);
            gl.viewport(0, 0, self.size.0, self.size.1);
            gl.use_program(Some(program));
            gl.color_mask(true, true, true, true);
        }
        self.bind(gl, program, held, scene)?;
        let mut unit = 0;
        for texture in &held.textures {
            let bound = self.engine(gl, texture.id)?;
            sampler(gl, program, &texture.name, unit, bound);
            unit += 1;
        }
        for structured in &held.structured {
            let bound = match structured.name.as_str() {
                TYPES => self.types(gl)?,
                _ => self.stand_in(gl)?,
            };
            sampler(gl, program, &structured.name, unit, bound);
            unit += 1;
        }
        unsafe {
            if let Some(location) = gl.get_uniform_location(program, "dx_Viewport") {
                gl.uniform_2_f32(Some(&location), self.size.0 as f32, self.size.1 as f32);
            }
            gl.bind_vertex_array(Some(layout));
            match volume {
                true => gl.draw_elements(
                    glow::TRIANGLES,
                    VOLUME_FACES.len() as i32,
                    glow::UNSIGNED_SHORT,
                    0,
                ),
                false => gl.draw_arrays(glow::TRIANGLES, 0, 3),
            }
            gl.bind_vertex_array(None);
        }
        Ok(())
    }

    /// The graph past the G-buffer: the view position off the depth, the light off both, and the
    /// frame off the light. Each pass is the package's own, run over the whole frame.
    fn resolve(
        &mut self,
        gl: &glow::Context,
        lighting: &Lighting,
        scene: &program::Scene,
        size: (i32, i32),
    ) -> Result<(), String> {
        let (position, _) = self.position.ok_or("no view position")?;
        let (light, _) = self.light.ok_or("no light buffer")?;
        let (lit, _) = self.lit.ok_or("no lit frame")?;
        self.size = size;
        unsafe {
            // A screen-wide pass covers every pixel and reads the depth rather than testing against
            // it, so nothing here is depth tested and nothing writes depth.
            gl.disable(glow::DEPTH_TEST);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
        }
        self.pass(gl, 0, &lighting.position, position, scene, false)?;

        // Every light adds to the same two buffers, so they start at nothing and each pass adds to
        // what the one before it left.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(light));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0, glow::COLOR_ATTACHMENT1]);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
        }
        self.pass(gl, 1, &lighting.directional, light, scene, false)?;
        self.pass(gl, 2, &lighting.point, light, scene, true)?;
        unsafe { gl.disable(glow::BLEND) };
        self.pass(gl, 3, &lighting.composite, lit, scene, false)
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
            // Point sampled: the shader addresses texel centers and mixes the pair itself, so a
            // filtered read would blend rows it never asked for.
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
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
        if let Err(why) = self.render(gl, painter, frame, meshes, size) {
            self.failure = Some(why);
            return;
        }
        self.failure = None;
        // Past the last channel is the lit frame, which is a buffer of its own rather than a page of
        // the G-buffer.
        let (read, at) = match frame.target >= TARGETS {
            true => (self.lit.map(|(frame, _)| frame), 0),
            false => (
                self.frames.get(frame.target / self.attachments).copied(),
                frame.target % self.attachments,
            ),
        };
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, read);
            gl.read_buffer(glow::COLOR_ATTACHMENT0 + at as u32);
            gl.blit_framebuffer(
                0,
                0,
                size.0,
                size.1,
                held.left_px,
                held.from_bottom_px,
                held.left_px + size.0,
                held.from_bottom_px + size.1,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            gl.viewport(held.left_px, held.from_bottom_px, size.0, size.1);
        }
    }

    fn render(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        meshes: &[Buffers],
        size: (i32, i32),
    ) -> Result<(), String> {
        // egui draws into whatever it bound before the callback, and the G-buffer has to go back to
        // it once the channel is on screen.
        let bound = unsafe { gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING) };
        self.attach(gl, size)?;
        let stand_in = self.stand_in(gl)?;
        // Only the callback knows how many pixels the widget really covers, and a screen-wide pass
        // has nothing else to turn a fragment into a texel with.
        let scene = program::Scene {
            size: (size.0 as f32, size.1 as f32),
            ..frame.scene
        };

        // The G-buffer a page at a time, and each page's depth pass before its buffer pass: the game
        // runs those as two passes over the whole draw rather than as two draws of one surface.
        for page in 0..self.frames.len() {
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.frames[page]));
                // A clear only reaches the attachments the draw buffers name, and a framebuffer
                // starts out naming its first alone: without this the rest keep whatever the
                // texture happened to be created holding.
                let width = (TARGETS - page * self.attachments).min(self.attachments);
                let attachments: Vec<u32> = (0..width)
                    .map(|at| glow::COLOR_ATTACHMENT0 + at as u32)
                    .collect();
                gl.draw_buffers(&attachments);
                gl.viewport(0, 0, size.0, size.1);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.disable(glow::BLEND);
                gl.enable(glow::DEPTH_TEST);
                gl.depth_func(glow::LEQUAL);
                gl.depth_mask(true);
                gl.color_mask(true, true, true, true);
                gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            }
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
                    let program = self.link(gl, (surface.material, depth, page), held)?;
                    unsafe {
                        gl.use_program(Some(program));
                        gl.depth_mask(depth);
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
                    self.bind(gl, program, held, &scene)?;
                    let mut unit = 0;
                    for texture in &held.textures {
                        let bound = match texture.id {
                            TABLE => match &shaded.table {
                                Some(table) => Some(self.table(gl, surface.material, table)?),
                                None => None,
                            },
                            id => shaded
                                .textures
                                .iter()
                                .find(|(held, _)| *held == id)
                                .and_then(|(_, held)| *held)
                                .and_then(|held| painter.texture(held)),
                        };
                        sampler(gl, program, &texture.name, unit, bound.unwrap_or(stand_in));
                        unit += 1;
                    }
                    // By name, not by position: a character's buffer pass reads the joint palette
                    // and the shader-type table both, and they hold different things.
                    for structured in &held.structured {
                        let bound = match structured.name.as_str() {
                            TYPES => self.types(gl)?,
                            _ => self.palette(gl, scene.view * scene.model)?,
                        };
                        sampler(gl, program, &structured.name, unit, bound);
                        unit += 1;
                    }
                    unsafe {
                        if let Some(location) = gl.get_uniform_location(program, "dx_Viewport") {
                            gl.uniform_2_f32(Some(&location), size.0 as f32, size.1 as f32);
                        }

                        gl.bind_vertex_array(Some(buffers.layout));
                        gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffers.vertices));
                        for location in 0..16 {
                            gl.disable_vertex_attrib_array(location);
                        }
                        for attribute in &held.attributes {
                            let Some((_, lanes, offset, kind)) = FIELDS
                                .iter()
                                .find(|(field, _, _, _)| *field == attribute.field)
                            else {
                                continue;
                            };
                            gl.enable_vertex_attrib_array(attribute.location);
                            let stride = size_of::<Vertex>() as i32;
                            match attribute.integer {
                                // A float pointer into an integer attribute is not a conversion, it
                                // is a different value, and nothing between here and the shader
                                // says so.
                                true => gl.vertex_attrib_pointer_i32(
                                    attribute.location,
                                    *lanes,
                                    *kind,
                                    stride,
                                    *offset,
                                ),
                                false => gl.vertex_attrib_pointer_f32(
                                    attribute.location,
                                    *lanes,
                                    *kind,
                                    *kind == glow::UNSIGNED_BYTE,
                                    stride,
                                    *offset,
                                ),
                            }
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
                }
            }
        }

        // Only where the lit frame is what is being shown: a raw channel is a page of the G-buffer
        // and owes nothing to the passes past it.
        if let Some(lighting) = frame.lighting.as_ref().filter(|_| frame.target >= TARGETS) {
            self.resolve(gl, lighting, &scene, size)?;
        }

        unsafe {
            gl.color_mask(true, true, true, true);
            gl.depth_mask(false);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, bound);
            // The default framebuffer draws to the back buffer and one of its own draws to its
            // first attachment; naming the wrong one is an error rather than a no-op.
            match bound {
                Some(_) => gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]),
                None => gl.draw_buffers(&[glow::BACK]),
            }
        }
        Ok(())
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.color.drain(..).map(Dead::Texture));
        dead.extend(self.tables.values().copied().map(Dead::Texture));
        dead.extend(
            [
                self.joints.take(),
                self.types.take(),
                self.stand_in.take(),
                self.unoccluded.take(),
                self.depth.take(),
            ]
            .into_iter()
            .flatten()
            .map(Dead::Texture),
        );
        dead.extend(self.frames.drain(..).map(Dead::Frame));
        for (frame, textures) in [
            self.position
                .take()
                .map(|(frame, held)| (frame, vec![held])),
            self.light
                .take()
                .map(|(frame, held)| (frame, held.to_vec())),
            self.lit.take().map(|(frame, held)| (frame, vec![held])),
        ]
        .into_iter()
        .flatten()
        {
            dead.push(Dead::Frame(frame));
            dead.extend(textures.into_iter().map(Dead::Texture));
        }
        if let Some((layout, held)) = self.screen.take() {
            dead.push(Dead::Layout(layout));
            dead.push(Dead::Buffer(held));
        }
        if let Some((layout, held, faces)) = self.volume.take() {
            dead.push(Dead::Layout(layout));
            dead.push(Dead::Buffer(held));
            dead.push(Dead::Buffer(faces));
        }
        dead.extend(self.blocks.drain(..).map(Dead::Buffer));
        dead.extend(
            std::mem::take(&mut self.programs)
                .into_values()
                .chain(std::mem::take(&mut self.deferred).into_values())
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

/// Point sampling and no wrap, which is what every buffer of the graph is read with: a shader
/// addresses texel centers and works out for itself what lies between them.
fn point(gl: &glow::Context) {
    for (name, value) in [
        (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
        (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
        (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
        (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
    ] {
        unsafe { gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32) };
    }
}

/// One buffer of the graph, the size of what is being drawn into.
fn plane(
    gl: &glow::Context,
    size: (i32, i32),
    internal: u32,
    format: u32,
    kind: u32,
) -> Result<glow::Texture, String> {
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            internal as i32,
            size.0,
            size.1,
            0,
            format,
            kind,
            glow::PixelUnpackData::Slice(None),
        );
        point(gl);
        Ok(texture)
    }
}

/// A framebuffer over the textures given, in the order a pass writes them.
fn frame_of(gl: &glow::Context, color: &[glow::Texture]) -> Result<glow::Framebuffer, String> {
    unsafe {
        let held = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(held));
        for (at, texture) in color.iter().enumerate() {
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0 + at as u32,
                glow::TEXTURE_2D,
                Some(*texture),
                0,
            );
        }
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        match status == glow::FRAMEBUFFER_COMPLETE {
            true => Ok(held),
            false => Err(format!(
                "a buffer of the graph would not complete: {status:#x}"
            )),
        }
    }
}

/// A buffer of dwords as the texture a shader reads it through, one dword to a texel.
fn dwords(gl: &glow::Context, values: &[u32]) -> Result<glow::Texture, String> {
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::R32UI as i32,
            program::ROW as i32,
            (values.len() / program::ROW) as i32,
            0,
            glow::RED_INTEGER,
            glow::UNSIGNED_INT,
            glow::PixelUnpackData::Slice(Some(bytemuck::cast_slice(values))),
        );
        point(gl);
        Ok(texture)
    }
}

/// Binds a texture to a unit and points the shader's own sampler at it.
fn sampler(
    gl: &glow::Context,
    program: glow::Program,
    name: &str,
    unit: u32,
    texture: glow::Texture,
) {
    unsafe {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        if let Some(location) = gl.get_uniform_location(program, name) {
            gl.uniform_1_i32(Some(&location), unit as i32);
        }
        // Nothing in GLSL says how many levels a texture has, so a shader that asks is told.
        if let Some(location) = gl.get_uniform_location(program, &format!("{name}_levels")) {
            gl.uniform_1_i32(Some(&location), 1);
        }
    }
}

/// The geometry the screen-wide passes and the point light's volume are drawn from, in one array
/// each so a pass can draw either without rebinding anything else.
fn upload_screen(gl: &glow::Context) -> Result<(glow::VertexArray, glow::Buffer), String> {
    unsafe {
        let layout = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(layout));
        let held = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(held));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(&SCREEN),
            glow::STATIC_DRAW,
        );
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 4, glow::FLOAT, false, 16, 0);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        Ok((layout, held))
    }
}

fn upload_volume(
    gl: &glow::Context,
) -> Result<(glow::VertexArray, glow::Buffer, glow::Buffer), String> {
    unsafe {
        let layout = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(layout));
        let held = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(held));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(&VOLUME),
            glow::STATIC_DRAW,
        );
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 4, glow::FLOAT, false, 16, 0);
        let faces = gl.create_buffer()?;
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(faces));
        gl.buffer_data_u8_slice(
            glow::ELEMENT_ARRAY_BUFFER,
            bytemuck::cast_slice(&VOLUME_FACES),
            glow::STATIC_DRAW,
        );
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        Ok((layout, held, faces))
    }
}

/// A one-texel texture answering with the same value everywhere.
fn flat(gl: &glow::Context, value: &[u8; 4]) -> Result<glow::Texture, String> {
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            1,
            1,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(value)),
        );
        for (name, held) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, held as i32);
        }
        Ok(texture)
    }
}

fn build_pair(gl: &glow::Context, vertex: &str, fragment: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut built = Vec::new();
        for (stage, source) in [
            (glow::VERTEX_SHADER, vertex),
            (glow::FRAGMENT_SHADER, fragment),
        ] {
            let shader = gl.create_shader(stage)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let why = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                for shader in built {
                    gl.delete_shader(shader);
                }
                gl.delete_program(program);
                return Err(why);
            }
            gl.attach_shader(program, shader);
            built.push(shader);
        }
        gl.link_program(program);
        for shader in built {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        if !gl.get_program_link_status(program) {
            let why = gl.get_program_info_log(program);
            gl.delete_program(program);
            return Err(why);
        }
        Ok(program)
    }
}

fn build(gl: &glow::Context) -> Result<glow::Program, String> {
    build_pair(gl, VERTEX_SOURCE, FRAGMENT_SOURCE)
}
