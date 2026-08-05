//! The GL side of the scene view.
//!
//! A zone is drawn with the game's own shaders, through the same frame the model viewer draws into.
//! What differs is that one model stands in many places, so a draw covers as many objects as the
//! package's own instancing buffer holds records for, and the rest of them follow in windows of the
//! same size.
//!
//! Those records carry each object into view space, so the whole buffer is written again whenever
//! the camera moves. It is written once a frame into a buffer of its own and each draw is pointed at
//! its own window of it, rather than uploaded per draw.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use glow::HasContext;

use super::super::super::mdl::deferred::{
    self, Buffers, Dead, LIT, Linked, TYPES, bury, graveyard, sampler,
};
use super::super::super::mdl::gpu::{Lighting, Shaded, attribute};
use super::super::super::mdl::{Vertex, program};

/// The color table, which the game's own shaders address as a texture of their own.
const TABLE: u32 = 0x2005_679f;

/// Models uploaded in one callback. Decoding is already spread over frames; this bounds the GL work
/// a single frame can be handed on top of it.
const UPLOADS: usize = 4;

/// What a uniform buffer's bound window has to start on until a context has been asked, which is the
/// largest any implementation is allowed to want.
const ALIGNMENT: i32 = 256;

/// One mesh's geometry.
struct Mesh {
    layout: glow::VertexArray,
    vertices: glow::Buffer,
    indices: glow::Buffer,
    count: i32,
}

/// One detail level of one model.
struct Level {
    meshes: Vec<Mesh>,
}

struct Model {
    levels: Vec<Level>,
}

/// One model's geometry, waiting for a context to upload it under.
pub struct Pending {
    pub model: usize,
    /// Per detail level, each mesh as its vertices and indices.
    pub levels: Vec<Vec<(Vec<Vertex>, Vec<u16>)>>,
}

/// What one mesh needs beyond its geometry.
pub struct Surface {
    pub material: usize,
    /// The material's own shaders, once its package has arrived.
    pub shaded: Option<Shaded>,
    pub cull: bool,
    /// Set where the material said the surface is not drawn at all.
    pub hidden: bool,
}

/// One model at one detail level, and where its objects stand.
pub struct Batch {
    pub model: usize,
    pub level: usize,
    pub instances: Vec<program::Instance>,
    /// One per mesh of the level, in the order they were uploaded.
    pub surfaces: Vec<Surface>,
}

pub struct Frame {
    pub scene: program::Scene,
    /// The passes that light the G-buffer, once their packages have arrived.
    pub lighting: Option<Arc<Lighting>>,
    /// Every light the zone places that reaches the frame.
    pub lamps: Vec<program::Lamp>,
    pub batches: Vec<Batch>,
}

pub struct Renderer {
    buffers: Buffers,
    models: Vec<Option<Model>>,
    pending: Vec<Pending>,
    /// One linked pair per material, pass and page of the G-buffer.
    programs: BTreeMap<(usize, bool, usize), Linked>,
    tables: BTreeMap<usize, glow::Texture>,
    /// Every object of the frame, in the layout the packages read them, and how far apart its
    /// windows sit.
    instances: Option<glow::Buffer>,
    alignment: i32,
    failure: Option<String>,
}

impl Renderer {
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            buffers: Buffers::default(),
            models: Vec::new(),
            pending: Vec::new(),
            programs: BTreeMap::new(),
            tables: BTreeMap::new(),
            instances: None,
            alignment: ALIGNMENT,
            failure: None,
        }))
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Geometry the card has not been handed yet, so the scene knows to keep asking for frames.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// How much of the G-buffer one pass can write, which decides how many readings a material's
    /// shaders are translated into.
    pub fn attachments(&self) -> usize {
        self.buffers.attachments()
    }

    pub fn queue_model(&mut self, pending: Pending) {
        self.pending.push(pending);
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
        for pending in self
            .pending
            .drain(..self.pending.len().min(UPLOADS))
            .collect::<Vec<_>>()
        {
            let at = pending.model;
            match upload(gl, pending) {
                Ok(model) => {
                    if self.models.len() <= at {
                        self.models.resize_with(at + 1, || None);
                    }
                    self.models[at] = Some(model);
                }
                Err(why) => log::error!("assets/layer: model {at}: {why}"),
            }
        }
        // egui draws into whatever it bound before the callback, and that has to be bound again
        // whether or not the frame drew. Asking the painter rather than the context is what makes
        // this work on the web: glow keeps its own map of the resources it created, and a
        // framebuffer read back out of WebGL is a JS object it cannot find in there.
        let bound = painter.intermediate_fbo();
        let held = info.viewport_in_pixels();
        let size = (held.width_px.max(1), held.height_px.max(1));
        let drawn = self.render(gl, painter, frame, size);
        let shown = self.buffers.show(
            gl,
            LIT,
            bound,
            (held.left_px, held.from_bottom_px, size.0, size.1),
        );
        self.failure = drawn.and(shown).err();
    }

    /// Every object of every batch, laid out so that one draw can be pointed at its own window.
    /// Answers where each batch's windows begin and how large one is.
    fn windows(
        &mut self,
        gl: &glow::Context,
        frame: &Frame,
        scene: &program::Scene,
    ) -> Result<(Vec<(i32, i32)>, i32), String> {
        if self.alignment == ALIGNMENT {
            let held = unsafe { gl.get_parameter_i32(glow::UNIFORM_BUFFER_OFFSET_ALIGNMENT) };
            self.alignment = held.clamp(1, ALIGNMENT);
        }
        let mut blob: Vec<u8> = Vec::new();
        let mut at: Vec<(i32, i32)> = Vec::new();
        let mut window = 0i32;
        for batch in &frame.batches {
            let held = batch
                .surfaces
                .iter()
                .filter_map(|surface| surface.shaded.as_ref())
                .filter_map(|shaded| shaded.buffer.first())
                .find_map(|held| held.instancing());
            let Some((buffer, count)) = held else {
                // A package that reads no instancing buffer is drawn one object at a time and
                // takes no window of its own.
                at.push((0, 0));
                continue;
            };
            at.push((
                blob.len() as i32,
                batch.instances.len().div_ceil(count) as i32,
            ));
            for held in batch.instances.chunks(count) {
                let mut bytes = buffer.fill(scene, program::Pass::Buffer, held);
                window = window.max(bytes.len() as i32);
                bytes.resize(aligned(bytes.len() as i32, self.alignment) as usize, 0);
                blob.extend(bytes);
            }
        }
        let held = match self.instances {
            Some(held) => held,
            None => {
                let held = unsafe { gl.create_buffer()? };
                self.instances = Some(held);
                held
            }
        };
        unsafe {
            gl.bind_buffer(glow::UNIFORM_BUFFER, Some(held));
            gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, &blob, glow::DYNAMIC_DRAW);
        }
        Ok((at, window))
    }

    fn render(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        size: (i32, i32),
    ) -> Result<(), String> {
        self.buffers.attach(gl, size)?;
        self.buffers.stand_ins(gl)?;
        let stand_in = self.buffers.stand_in(gl)?;
        // Only the callback knows how many pixels the widget really covers, and a screen-wide pass
        // has nothing else to turn a fragment into a texel with.
        let scene = program::Scene {
            size: (size.0 as f32, size.1 as f32),
            ..frame.scene
        };
        let (offsets, window) = self.windows(gl, frame, &scene)?;
        let instances = self.instances.ok_or("no instance buffer")?;

        for page in 0..self.buffers.pages() {
            self.buffers.open(gl, page);
            for depth in [true, false] {
                for (batch, (offset, windows)) in frame.batches.iter().zip(&offsets) {
                    // Taken by value first: the draw wants the frame's own buffers mutably, and
                    // the models would still be borrowed.
                    let held: Vec<(glow::VertexArray, glow::Buffer, i32)> = match self
                        .models
                        .get(batch.model)
                        .and_then(Option::as_ref)
                        .and_then(|model| model.levels.get(batch.level))
                    {
                        Some(level) => level
                            .meshes
                            .iter()
                            .map(|mesh| (mesh.layout, mesh.vertices, mesh.count))
                            .collect(),
                        None => continue,
                    };
                    for (mesh, surface) in held.iter().zip(&batch.surfaces) {
                        if surface.hidden {
                            continue;
                        }
                        let Some(shaded) = &surface.shaded else {
                            continue;
                        };
                        let held = match depth {
                            true => shaded.depth.as_deref(),
                            false => shaded.buffer.get(page).map(Arc::as_ref),
                        };
                        let Some(held) = held.filter(|held| depth || !held.targets.is_empty())
                        else {
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
                            // A material with no depth pass writes its own, since the depth buffer
                            // is what says which pixels the frame covered.
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
                        if held.batch() > 1 {
                            self.buffers.bind(gl, program, held, &scene, &[])?;
                        }
                        // Before anything is bound: making a texture binds it to whichever unit
                        // happens to be active, so one made partway through the loop below takes
                        // over the unit the sampler before it was just given.
                        let table = match &shaded.table {
                            Some(table) => Some(self.table(gl, surface.material, table)?),
                            None => None,
                        };
                        let mut unit = 0;
                        for texture in &held.textures {
                            let bound = match texture.id {
                                TABLE => table,
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
                        for structured in &held.structured {
                            let bound = match structured.name.as_str() {
                                TYPES => self.buffers.types(gl)?,
                                _ => stand_in,
                            };
                            sampler(gl, program, &structured.name, unit, bound);
                            unit += 1;
                        }
                        let slot = held
                            .buffers
                            .iter()
                            .position(|buffer| buffer.instances() > 1)
                            .unwrap_or(0) as u32;
                        unsafe {
                            if let Some(location) = gl.get_uniform_location(program, "dx_Viewport")
                            {
                                gl.uniform_2_f32(Some(&location), size.0 as f32, size.1 as f32);
                            }
                            gl.bind_vertex_array(Some(mesh.0));
                            gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh.1));
                            for location in 0..16 {
                                gl.disable_vertex_attrib_array(location);
                            }
                            for held in &held.attributes {
                                attribute(gl, held);
                            }
                            let count = held.batch() as i32;
                            for at in 0..*windows {
                                gl.bind_buffer_range(
                                    glow::UNIFORM_BUFFER,
                                    slot,
                                    Some(instances),
                                    offset + at * aligned(window, self.alignment),
                                    window,
                                );
                                let drawn = (batch.instances.len() as i32 - at * count).min(count);
                                gl.draw_elements_instanced(
                                    glow::TRIANGLES,
                                    mesh.2,
                                    glow::UNSIGNED_SHORT,
                                    0,
                                    drawn,
                                );
                            }
                            gl.bind_vertex_array(None);
                        }
                        // A package that reads no instancing buffer draws one object at a time,
                        // off the transform the scene carries.
                        if held.batch() == 1 {
                            for instance in &batch.instances {
                                let held_scene = program::Scene {
                                    model: instance.transform,
                                    ..scene
                                };
                                self.buffers.bind(gl, program, held, &held_scene, &[])?;
                                unsafe {
                                    gl.bind_vertex_array(Some(mesh.0));
                                    gl.draw_elements(
                                        glow::TRIANGLES,
                                        mesh.2,
                                        glow::UNSIGNED_SHORT,
                                        0,
                                    );
                                    gl.bind_vertex_array(None);
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(lighting) = frame.lighting.as_ref() {
            self.buffers.resolve(gl, lighting, &scene, &frame.lamps)?;
        }
        Ok(())
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
            deferred::point(gl);
            self.tables.insert(material, texture);
            Ok(texture)
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        graveyard().lock().unwrap().extend(
            self.models
                .drain(..)
                .flatten()
                .flat_map(|model| model.levels)
                .flat_map(|level| {
                    level.meshes.into_iter().flat_map(|mesh| {
                        [
                            Dead::Layout(mesh.layout),
                            Dead::Buffer(mesh.vertices),
                            Dead::Buffer(mesh.indices),
                        ]
                    })
                })
                .chain(self.instances.take().map(Dead::Buffer))
                .chain(self.tables.values().copied().map(Dead::Texture))
                .chain(
                    std::mem::take(&mut self.programs)
                        .into_values()
                        .map(|held| Dead::Program(held.program)),
                ),
        );
    }
}

/// The next offset a uniform buffer will let a window start on.
fn aligned(bytes: i32, alignment: i32) -> i32 {
    let held = alignment.max(1);
    (bytes + held - 1) / held * held
}

fn upload(gl: &glow::Context, pending: Pending) -> Result<Model, String> {
    let mut levels = Vec::new();
    for meshes in pending.levels {
        let mut built = Vec::new();
        for (vertices, indices) in &meshes {
            built.push(upload_mesh(gl, vertices, indices)?);
        }
        levels.push(Level { meshes: built });
    }
    unsafe { gl.bind_buffer(glow::ARRAY_BUFFER, None) };
    Ok(Model { levels })
}

/// One mesh's buffers, with its own vertex array. The array is not an optimization: egui leaves its
/// own bound while a callback runs, so setting attribute pointers without one would rewrite egui's
/// layout to point at scene geometry.
fn upload_mesh(gl: &glow::Context, vertices: &[Vertex], indices: &[u16]) -> Result<Mesh, String> {
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

        let drawn = gl.create_buffer()?;
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(drawn));
        gl.buffer_data_u8_slice(
            glow::ELEMENT_ARRAY_BUFFER,
            bytemuck::cast_slice(indices),
            glow::STATIC_DRAW,
        );

        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        Ok(Mesh {
            layout,
            vertices: held,
            indices: drawn,
            count: indices.len() as i32,
        })
    }
}
