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
    self, Buffers, Dead, LIT, Layered, Linked, TYPES, bury, graveyard, sampler,
};
use super::super::super::mdl::gpu::{
    Exposure, Lighting, Occlusion, Shaded, Smoothing, attribute,
};
use super::super::super::mdl::{Vertex, program};

/// The color table, which the game's own shaders address as a texture of their own.
const TABLE: u32 = 0x2005_679f;

/// Models uploaded in one callback. Decoding is already spread over frames; this bounds the GL work
/// a single frame can be handed on top of it.
const UPLOADS: usize = 4;

/// What a uniform buffer's bound window has to start on until a context has been asked, which is the
/// largest any implementation is allowed to want.
const ALIGNMENT: i32 = 256;

/// The page a linked program is keyed under for the sun's own pass, past any the G-buffer takes.
const SUN: usize = usize::MAX;

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

impl Model {
    fn dead(self) -> impl Iterator<Item = Dead> {
        self.levels.into_iter().flat_map(|level| {
            level.meshes.into_iter().flat_map(|mesh| {
                [
                    Dead::Layout(mesh.layout),
                    Dead::Buffer(mesh.vertices),
                    Dead::Buffer(mesh.indices),
                ]
            })
        })
    }
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
    /// The chain that brings what they resolve into the range a screen holds, on the same terms.
    pub exposure: Option<Arc<Exposure>>,
    /// The pass that fills whatever the frame did not cover, once its shader has arrived.
    pub skybox: Option<Arc<program::Program>>,
    pub sunlight: Option<Arc<program::Program>>,
    /// The one that fades what is far away toward the weather's own fog, and then toward that sky.
    pub haze: Option<Arc<program::Program>>,
    /// The two draws that put clouds over that sky, the horizon band first.
    pub clouds: [Option<Arc<program::Program>>; 2],
    /// The pair that smooths its edges, and the chain that weights every light by how much sky
    /// reaches the pixel.
    pub smoothing: Option<Arc<Smoothing>>,
    pub occlusion: Option<Arc<Occlusion>>,
    /// Every light the zone places that reaches the frame.
    pub lamps: Vec<program::Lamp>,
    pub batches: Vec<Batch>,
}

pub struct Renderer {
    buffers: Buffers,
    models: Vec<Option<Model>>,
    pending: Vec<Pending>,
    /// The game's own textures the shaders read that no material names, waiting for a context.
    supplied: Vec<(u32, Layered)>,
    /// The two cloud textures the weather names, the same way.
    overcast: Vec<(usize, String, Layered)>,
    /// The table the shading passes index, waiting for a context.
    types: Option<Vec<u32>>,
    /// One linked pair per material, pass and page of the G-buffer.
    programs: BTreeMap<(usize, bool, usize), Linked>,
    tables: BTreeMap<usize, glow::Texture>,
    /// Every object of the frame, in the layout the packages read them, and how far apart its
    /// windows sit.
    instances: Option<glow::Buffer>,
    /// The same records as the sun sees them, since each holds its object taken through the view.
    shadow_instances: Option<glow::Buffer>,
    alignment: i32,
    failure: Option<String>,
}

impl Renderer {
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            buffers: Buffers::default(),
            models: Vec::new(),
            pending: Vec::new(),
            supplied: Vec::new(),
            overcast: Vec::new(),
            types: None,
            programs: BTreeMap::new(),
            tables: BTreeMap::new(),
            instances: None,
            shadow_instances: None,
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

    /// The exposure the last frame settled on, which is a reading of the frame and not of a file.
    pub fn exposed(&self) -> f32 {
        self.buffers.exposed()
    }

    /// The luminance that exposure was worked out from, which is what the exposure alone cannot
    /// show once it sits on either end of the range the file states.
    pub fn measured(&self) -> f32 {
        self.buffers.measured()
    }

    /// Which of the passes past the lighting ran over the last frame.
    pub fn drawn(&self) -> deferred::Drawn {
        self.buffers.drawn()
    }

    pub fn queue_model(&mut self, pending: Pending) {
        self.pending.push(pending);
    }

    /// Hands one of the game's own textures over, under the resource id its shaders name it by.
    pub fn queue_supplied(&mut self, id: u32, held: Layered) {
        self.supplied.push((id, held));
    }

    /// The same for one of the two cloud draws, which read a texture apiece under one name.
    pub fn queue_overcast(&mut self, at: usize, path: String, held: Layered) {
        self.overcast.push((at, path, held));
    }

    pub fn queue_types(&mut self, values: Vec<u32>) {
        self.types = Some(values);
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
                    // A model is read again at a finer level once the eye comes close enough, so
                    // what it stood as goes the way of anything else the card is done with.
                    if let Some(held) = self.models[at].replace(model) {
                        graveyard().lock().unwrap().extend(held.dead());
                    }
                }
                Err(why) => log::error!("assets/layer: model {at}: {why}"),
            }
        }
        for (id, held) in std::mem::take(&mut self.supplied) {
            if let Err(why) = self.buffers.layered(gl, id, &held) {
                log::error!("assets/layer: texture {id:#x}: {why}");
            }
        }
        for (at, path, held) in std::mem::take(&mut self.overcast) {
            if let Err(why) = self.buffers.overcast(gl, at, &path, &held) {
                log::error!("assets/layer: {path}: {why}");
            }
        }
        if let Some(values) = self.types.take()
            && let Err(why) = self.buffers.fill_types(gl, &values)
        {
            log::error!("assets/layer: shader types: {why}");
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
        lit: bool,
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
        // The record holds `view * transform`, so the sun's own pass cannot share the frame's.
        let into = match lit {
            true => &mut self.instances,
            false => &mut self.shadow_instances,
        };
        let held = match *into {
            Some(held) => held,
            None => {
                let held = unsafe { gl.create_buffer()? };
                *into = Some(held);
                held
            }
        };
        unsafe {
            gl.bind_buffer(glow::UNIFORM_BUFFER, Some(held));
            gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, &blob, glow::DYNAMIC_DRAW);
        }
        Ok((at, window))
    }

    /// The scene's depth as the sun sees it, which the lighting tests a pixel against. Depth only:
    /// no colour is written, no texture is bound, and a material that answers no shadow subview is
    /// left out rather than drawn with the wrong pass.
    fn shadow(
        &mut self,
        gl: &glow::Context,
        frame: &Frame,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let Some((into, _)) = self.buffers.shadow() else {
            return Ok(());
        };
        let (view, projection) = program::shadow_camera(scene.light, scene.view);
        let sun = program::Scene {
            view,
            projection,
            ..scene.clone()
        };
        let (offsets, window) = self.windows(gl, frame, &sun, false)?;
        let instances = self.shadow_instances.ok_or("no shadow instance buffer")?;
        let size = self.buffers.shadow_size();
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.viewport(0, 0, size, size);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.draw_buffers(&[glow::NONE]);
            gl.clear_color(1.0, 1.0, 1.0, 1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            // The sun looks along its own axis, so what faces away from it is what casts: culling
            // the near side is what keeps a surface from shadowing itself along every edge.
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::FRONT);
            gl.front_face(glow::CCW);
        }
        for (batch, (offset, windows)) in frame.batches.iter().zip(&offsets) {
            let meshes: Vec<(glow::VertexArray, glow::Buffer, i32)> = match self
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
            for (mesh, surface) in meshes.iter().zip(&batch.surfaces) {
                if surface.hidden {
                    continue;
                }
                let Some(held) = surface
                    .shaded
                    .as_ref()
                    .and_then(|shaded| shaded.shadow.as_deref())
                else {
                    continue;
                };
                let program =
                    deferred::link(gl, &mut self.programs, (surface.material, true, SUN), held)?;
                unsafe { gl.use_program(Some(program)) };
                if held.batch() > 1 {
                    self.buffers.bind(gl, program, held, &sun, &[])?;
                }
                let slot = held
                    .buffers
                    .iter()
                    .position(|buffer| buffer.instances() > 1)
                    .unwrap_or(0) as u32;
                unsafe {
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
            }
        }
        unsafe {
            gl.cull_face(glow::BACK);
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
        }
        Ok(())
    }

    /// Every surface that lights itself, drawn over the frame the composite left. Water and the
    /// glass a zone places fill the G-buffer through a semitransparent pass and answer into the lit
    /// frame here, against the depth their own buffer pass settled.
    fn blended(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
        offsets: &[(i32, i32)],
        window: i32,
    ) -> Result<(), String> {
        let wanted = frame.batches.iter().any(|batch| {
            batch
                .surfaces
                .iter()
                .any(|surface| surface.shaded.as_ref().is_some_and(|held| held.resolve.is_some()))
        });
        if !wanted {
            return Ok(());
        }
        let into = self.buffers.frame().ok_or("no lit frame")?;
        let instances = self.instances.ok_or("no instance buffer")?;
        let stand_in = self.buffers.stand_in(gl)?;
        let size = self.buffers.size();
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, size.0, size.1);
            gl.color_mask(true, true, true, true);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            // Against what its own buffer pass settled, so a surface layered over itself keeps the
            // fragment the G-buffer kept.
            gl.depth_func(glow::EQUAL);
            gl.depth_mask(false);
        }
        for (batch, (offset, windows)) in frame.batches.iter().zip(offsets) {
            let meshes: Vec<(glow::VertexArray, glow::Buffer, i32)> = match self
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
            for (mesh, surface) in meshes.iter().zip(&batch.surfaces) {
                if surface.hidden {
                    continue;
                }
                let Some(shaded) = &surface.shaded else {
                    continue;
                };
                let Some(held) = shaded.resolve.as_deref() else {
                    continue;
                };
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
                if held.batch() > 1 {
                    self.buffers.bind(gl, program, held, scene, &[])?;
                }
                let table = match &shaded.table {
                    Some(table) => Some(self.table(gl, surface.material, table)?),
                    None => None,
                };
                let mut unit = 0;
                for texture in &held.textures {
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
            }
        }
        unsafe {
            gl.depth_func(glow::LESS);
            gl.disable(glow::DEPTH_TEST);
        }
        Ok(())
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
            ..frame.scene.clone()
        };
        let (offsets, window) = self.windows(gl, frame, &scene, true)?;
        let instances = self.instances.ok_or("no instance buffer")?;
        self.shadow(gl, frame, &scene)?;

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
                            // Only a plane can come from the material: what it binds is an egui
                            // texture, and egui has nothing but two-dimensional ones.
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
                                    ..scene.clone()
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
            // Before anything reads it: every lighting pass and the composite take the occlusion as
            // a weight on what they work out.
            match frame.occlusion.as_ref() {
                Some(held) => self.buffers.occlude(gl, held, &scene)?,
                None => self.buffers.unocclude(),
            }
            // Before the lighting too: the shadowed variant reads the mask as a weight on what it
            // works out, so it has to be standing before the first light is resolved.
            match lighting.shadow.as_deref() {
                Some(held) => self.buffers.shade(gl, held, &scene)?,
                None => self.buffers.unshade(),
            }
            self.buffers.resolve(gl, lighting, &scene, &frame.lamps)?;
            // Before the exposure, which reads the whole frame: a black hole where the sky belongs
            // measures as a far darker scene than it is.
            if let Some(skybox) = frame.skybox.as_ref() {
                self.buffers.sky(gl, skybox, &scene)?;
                // Over the sky and under the clouds, which is where a real frame draws it.
                if let Some(held) = frame.sunlight.as_ref() {
                    self.buffers.sun(gl, held, &scene)?;
                }
                // Over the sky and under everything the frame covered, the sheet first so that the
                // band stands in front of it where the two meet at the horizon.
                for (at, held) in frame.clouds.iter().enumerate().rev() {
                    let Some(held) = held else { continue };
                    let scene = program::Scene {
                        model: program::Cloud::placement(
                            match at {
                                0 => program::Pass::CloudBand,
                                _ => program::Pass::CloudSheet,
                            },
                            scene.view.inverse().w_axis.truncate(),
                        ),
                        projection: program::Cloud::frustum(&scene),
                        ..scene.clone()
                    };
                    self.buffers.cloud(gl, at, held, &scene)?;
                }
                // After both, which is what it fades the far distance toward, and before the
                // exposure, which measures the frame the fog leaves rather than the one under it.
                self.blended(gl, painter, frame, &scene, &offsets, window)?;
                if let Some(haze) = frame.haze.as_ref() {
                    self.buffers.fog(gl, haze, &scene)?;
                }
            }
            if let Some(exposure) = frame.exposure.as_ref() {
                self.buffers.expose(gl, exposure, &scene)?;
            }
            if let Some(smoothing) = frame.smoothing.as_ref() {
                self.buffers.antialias(gl, smoothing, &scene)?;
            }
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
                .flat_map(Model::dead)
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
