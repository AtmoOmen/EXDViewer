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
use half::f16;

use super::super::super::avfx::{gpu as avfxgpu, program as avfxprogram, sim as avfxsim};
use super::super::super::mdl::deferred::{
    self, Buffers, Dead, LIT, Layered, Linked, TYPES, bury, graveyard, sampler,
};
use super::super::super::mdl::gpu::{
    Bound, Exposure, Glare, Lighting, Occlusion, Shaded, Smoothing, attribute,
};
use super::super::super::mdl::{self, Vertex, program};

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

/// The material slot the grass readings are keyed under, which no material of a zone reaches.
const TURF: usize = usize::MAX;

/// The pages a fringe leg is keyed under, past any the G-buffer or its resolve take.
const SHEER_BUFFER: usize = deferred::REFLECTED + 1;
const SHEER_RESOLVE: usize = SHEER_BUFFER + 1;

/// The color map a blade is cut out of, which the zone's own grass file names per auto layer.
const COLOR_MAP: u32 = 0x6e1d_f4a2;

/// One mesh's geometry, and one vertex array per set of attributes read off it. The pointers are
/// the array's own state and the passes drawing a mesh do not read the same set, so each set has an
/// array of its own rather than one array being pointed again per pass.
struct Mesh {
    vertices: glow::Buffer,
    indices: glow::Buffer,
    count: i32,
    arrays: Vec<(Vec<program::Attribute>, glow::VertexArray)>,
}

impl Mesh {
    fn array(
        &mut self,
        gl: &glow::Context,
        attributes: &[program::Attribute],
    ) -> Result<glow::VertexArray, String> {
        pointed(
            gl,
            &mut self.arrays,
            (self.vertices, self.indices),
            attributes,
            attribute,
        )
    }

    fn dead(self) -> impl Iterator<Item = Dead> {
        [Dead::Buffer(self.vertices), Dead::Buffer(self.indices)]
            .into_iter()
            .chain(self.arrays.into_iter().map(|(_, layout)| Dead::Layout(layout)))
    }
}

/// One corner of one blade, in the layout the grass package's own vertex shader reads. The last lane
/// of the position is what that shader adds one to for the homogeneous coordinate, and the first of
/// `color1` offsets the coordinate onto the tile the placement's profile names.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Corner {
    pub position: [f16; 4],
    pub uv: [f16; 4],
    pub color1: [f16; 4],
    /// A texel of the gradation map, and the weight the tint off it is mixed at.
    pub color: [u8; 4],
}

/// Where each semantic the grass package reads sits in one of those.
const CORNERS: [(program::Field, i32, i32, u32); 4] = [
    (program::Field::Position, 4, 0, glow::HALF_FLOAT),
    (program::Field::Uv, 4, 8, glow::HALF_FLOAT),
    (program::Field::Color1, 4, 16, glow::HALF_FLOAT),
    (program::Field::Color, 4, 24, glow::UNSIGNED_BYTE),
];

/// One grid's blades at one auto layer, as the card holds them, with an array per set of attributes
/// for the same reason a mesh has one.
struct Turf {
    vertices: glow::Buffer,
    indices: glow::Buffer,
    count: i32,
    arrays: Vec<(Vec<program::Attribute>, glow::VertexArray)>,
}

impl Turf {
    fn array(
        &mut self,
        gl: &glow::Context,
        attributes: &[program::Attribute],
    ) -> Result<glow::VertexArray, String> {
        pointed(
            gl,
            &mut self.arrays,
            (self.vertices, self.indices),
            attributes,
            corner,
        )
    }

    fn dead(self) -> impl Iterator<Item = Dead> {
        [Dead::Buffer(self.vertices), Dead::Buffer(self.indices)]
            .into_iter()
            .chain(self.arrays.into_iter().map(|(_, layout)| Dead::Layout(layout)))
    }
}

/// Blades waiting for a context to upload them under.
pub struct Sown {
    pub turf: usize,
    pub corners: Vec<Corner>,
    pub indices: Vec<u32>,
}

/// The two readings the zone's grass is drawn with, each one page of the G-buffer's targets. The
/// first writes the albedo and settles the depth; the second fills the channels it left at nought.
pub struct Grass {
    pub buffer: Vec<Arc<program::Program>>,
    pub normal: Vec<Arc<program::Program>>,
}

/// One grid's blades at one auto layer, and what they are drawn against.
pub struct Blades {
    pub turf: usize,
    /// Where the grid stands, which its placements are measured from.
    pub origin: glam::Vec3,
    pub color_map: egui::TextureId,
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
        self.levels
            .into_iter()
            .flat_map(|level| level.meshes.into_iter().flat_map(Mesh::dead))
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
    /// Whether the model this belongs to is drawn through the wind's own reading of the material,
    /// which is a second pair of shaders off the same package.
    pub waving: bool,
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
    /// How many of them lead, being the ones the sun's own pass draws. An instance states for
    /// itself whether it casts, and those that do are laid out first.
    pub casts: usize,
    /// One per mesh of the level, in the order they were uploaded.
    pub surfaces: Vec<Surface>,
}

/// Every placement of one `.avfx` file, already merged into batches: the particles a shared
/// simulation stepped, each copy carried into the world by its own placement's transform.
pub struct EffectDraw {
    pub path: String,
    pub batches: Vec<avfxgpu::Batch>,
    /// The file's own `SPFR`, which the soft-particle apricot_model variant reads as its fade
    /// range: every placement of one file shares the same effect, so this is the effect's, not the
    /// placement's.
    pub fade_range: f32,
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
    pub moonlight: Option<Arc<program::Program>>,
    pub starlight: Option<Arc<program::Program>>,
    /// The one that fades what is far away toward the weather's own fog, and then toward that sky.
    pub haze: Option<Arc<program::Program>>,
    /// The two draws that put clouds over that sky, the horizon band first.
    pub clouds: [Option<Arc<program::Program>>; 2],
    /// The sheet drawn again from where the sun stands, and the blur the map it fills is left in.
    pub cloud_shadow: Option<(Arc<program::Program>, Arc<program::Program>)>,
    /// The chain that spreads the bright end of the frame into a halo, once its four shaders have
    /// arrived.
    pub glare: Option<Arc<Glare>>,
    /// The pair that smooths its edges, and the chain that weights every light by how much sky
    /// reaches the pixel.
    pub smoothing: Option<Arc<Smoothing>>,
    pub occlusion: Option<Arc<Occlusion>>,
    /// The chain that reflects the frame off itself.
    pub reflection: Option<Arc<deferred::Reflection>>,
    /// The one that fills what water reflects itself through.
    pub water_mirror: Option<Arc<deferred::WaterMirror>>,
    /// The one that darkens its corners, which runs after all of them.
    pub vignette: Option<Arc<program::Program>>,
    /// Every light the zone places that reaches the frame.
    pub lamps: Vec<program::Lamp>,
    pub batches: Vec<Batch>,
    /// The characters standing in the scene, each drawn from a model of its own: a character is
    /// assembled rather than placed, so it fills the same G-buffer through its own geometry instead
    /// of through an instance of the scene's.
    pub casts: Vec<mdl::Cast>,
    /// The zone's own grass, once its package has arrived, and the grids it stands over.
    pub grass: Option<Arc<Grass>>,
    pub blades: Vec<Blades>,
    /// Every placed effect that has arrived, textured and stepped to where the clock stands.
    pub effects: Vec<EffectDraw>,
    /// The two apricot packages every one of them is drawn with.
    pub effect_packages: Arc<avfxgpu::Packages>,
}

pub struct Renderer {
    buffers: Buffers,
    models: Vec<Option<Model>>,
    pending: Vec<Pending>,
    turf: BTreeMap<usize, Turf>,
    sown: Vec<Sown>,
    /// The game's own textures the shaders read that no material names, waiting for a context.
    supplied: Vec<(u32, Layered)>,
    /// The two cloud textures the weather names, the same way.
    overcast: Vec<(usize, String, Layered)>,
    /// The star field's own three, waiting for a context.
    starlit: Vec<(usize, Layered)>,
    /// The textures the zone's own materials name that egui cannot hold, under the paths naming
    /// them.
    stacks: Vec<(Arc<str>, Layered)>,
    /// The table the shading passes index, waiting for a context.
    types: Option<Vec<u32>>,
    /// One linked pair per material, pass and page of the G-buffer.
    programs: BTreeMap<(usize, bool, bool, usize), Linked>,
    /// The two members of water's own reflection chain drawn over the water itself, which are one
    /// program each however many materials the zone's water is made of.
    water_programs: BTreeMap<usize, Linked>,
    tables: BTreeMap<usize, glow::Texture>,
    /// Every object of the frame, in the layout the packages read them, and how far apart its
    /// windows sit.
    instances: Option<glow::Buffer>,
    /// The same records as the sun sees them, since each holds its object taken through the view.
    shadow_instances: Option<glow::Buffer>,
    alignment: i32,
    failure: Option<String>,
    /// One entry per unique effect file, uploaded once and drawn once per frame under however many
    /// placements share it.
    effects: BTreeMap<String, avfxgpu::Particles>,
    pending_effects: Vec<(String, Vec<avfxsim::Mesh>)>,
}

impl Renderer {
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            buffers: Buffers::default(),
            models: Vec::new(),
            pending: Vec::new(),
            turf: BTreeMap::new(),
            sown: Vec::new(),
            supplied: Vec::new(),
            overcast: Vec::new(),
            starlit: Vec::new(),
            stacks: Vec::new(),
            types: None,
            programs: BTreeMap::new(),
            water_programs: BTreeMap::new(),
            tables: BTreeMap::new(),
            instances: None,
            shadow_instances: None,
            alignment: ALIGNMENT,
            failure: None,
            effects: BTreeMap::new(),
            pending_effects: Vec::new(),
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

    pub fn queue_turf(&mut self, sown: Sown) {
        self.sown.push(sown);
    }

    /// Hands one of the game's own textures over, under the resource id its shaders name it by.
    /// A texture one of the zone's own materials names, under the path that named it.
    pub fn queue_stack(&mut self, path: Arc<str>, held: Layered) {
        self.stacks.push((path, held));
    }

    pub fn queue_supplied(&mut self, id: u32, held: Layered) {
        self.supplied.push((id, held));
    }

    /// The same for one of the two cloud draws, which read a texture apiece under one name.
    pub fn queue_overcast(&mut self, at: usize, path: String, held: Layered) {
        self.overcast.push((at, path, held));
    }

    /// One of the star field's three textures, under the index its draw knows it by.
    pub fn queue_starlit(&mut self, at: usize, held: Layered) {
        self.starlit.push((at, held));
    }

    pub fn queue_types(&mut self, values: Vec<u32>) {
        self.types = Some(values);
    }

    /// A newly parsed `.avfx`'s own models, once for whichever path names it: a placement that
    /// shares the path draws the same upload again rather than asking for a second one.
    pub fn queue_effect(&mut self, path: String, models: Vec<avfxsim::Mesh>) {
        self.pending_effects.push((path, models));
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
        for (path, models) in std::mem::take(&mut self.pending_effects) {
            self.effects
                .entry(path)
                .or_insert_with(|| avfxgpu::Particles::new_plain(models));
        }
        for sown in std::mem::take(&mut self.sown) {
            let at = sown.turf;
            match upload_turf(gl, &sown.corners, &sown.indices) {
                Ok(turf) => {
                    if let Some(stale) = self.turf.insert(at, turf) {
                        graveyard().lock().unwrap().extend(stale.dead());
                    }
                }
                Err(why) => log::error!("assets/layer: grass {at}: {why}"),
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
        for (at, held) in std::mem::take(&mut self.starlit) {
            if let Err(why) = self.buffers.starlit(gl, at, &held) {
                log::error!("assets/layer: star texture {at}: {why}");
            }
        }
        for (path, held) in std::mem::take(&mut self.stacks) {
            if let Err(why) = self.buffers.stack(gl, &path, &held) {
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

    /// The array one mesh of a batch is drawn from for the attributes a pass reads off it.
    fn array(
        &mut self,
        gl: &glow::Context,
        batch: &Batch,
        mesh: usize,
        attributes: &[program::Attribute],
    ) -> Result<Option<glow::VertexArray>, String> {
        self.models
            .get_mut(batch.model)
            .and_then(Option::as_mut)
            .and_then(|model| model.levels.get_mut(batch.level))
            .and_then(|level| level.meshes.get_mut(mesh))
            .map(|mesh| mesh.array(gl, attributes))
            .transpose()
    }

    /// Every object of every batch, laid out so that one draw can be pointed at its own window.
    /// Answers where each batch's windows begin, how many it takes and how large one is. The size
    /// is the batch's own record rather than the frame's largest: read back wider than it was
    /// written, a window lands on the next batch's bytes.
    fn windows(
        &mut self,
        gl: &glow::Context,
        frame: &Frame,
        scene: &program::Scene,
        lit: bool,
    ) -> Result<Vec<(i32, i32, i32)>, String> {
        if self.alignment == ALIGNMENT {
            let held = unsafe { gl.get_parameter_i32(glow::UNIFORM_BUFFER_OFFSET_ALIGNMENT) };
            self.alignment = held.clamp(1, ALIGNMENT);
        }
        let mut blob: Vec<u8> = Vec::new();
        let mut at: Vec<(i32, i32, i32)> = Vec::new();
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
                at.push((0, 0, 0));
                continue;
            };
            let drawn = match lit {
                true => batch.instances.len(),
                false => batch.casts,
            };
            let offset = blob.len() as i32;
            let mut window = 0;
            for held in batch.instances[..drawn].chunks(count) {
                let mut bytes = buffer.fill(scene, program::Pass::Buffer, held);
                window = bytes.len() as i32;
                bytes.resize(aligned(window, self.alignment) as usize, 0);
                blob.extend(bytes);
            }
            at.push((offset, drawn.div_ceil(count) as i32, window));
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
        Ok(at)
    }

    /// Every sampler a pass declares, pointed at what the material named for it, at what the engine
    /// binds where nothing names one, or at the stand-in.
    fn textures(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        program: glow::Program,
        held: &program::Program,
        shaded: &Shaded,
        material: usize,
    ) -> Result<(), String> {
        let stand_in = self.buffers.stand_in(gl)?;
        // Before anything is bound: making a texture binds it to whichever unit happens to be
        // active, so one made partway through the loop below takes over the unit the sampler
        // before it was just given.
        let table = match &shaded.table {
            Some(table) => Some(self.table(gl, material, table)?),
            None => None,
        };
        let mut unit = 0;
        for texture in &held.textures {
            // Only a plane can come from the material: what it binds is an egui texture, and egui
            // has nothing but two-dimensional ones.
            let mut aniso = 0.0;
            let bound = match texture.kind {
                program::Kind::Plane => {
                    let held = match texture.id {
                        TABLE => table,
                        id => {
                            let plane = shaded.bound(id).and_then(Bound::plane);
                            aniso = plane.map_or(0.0, |(_, aniso)| aniso);
                            plane.and_then(|(held, _)| painter.texture(held))
                        }
                    };
                    match held {
                        Some(held) => held,
                        None => self.buffers.engine(gl, texture.id)?,
                    }
                }
                kind => match shaded
                    .bound(texture.id)
                    .and_then(Bound::stacked)
                    .and_then(|path| self.buffers.stacked(kind, path))
                {
                    Some(held) => held,
                    None => self.buffers.absent(gl, kind, texture.id)?,
                },
            };
            deferred::bind(
                gl,
                program,
                &texture.name,
                unit,
                bound,
                deferred::target(texture.kind),
                aniso,
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
        Ok(())
    }

    /// The scene's depth as the sun sees it, which the lighting tests a pixel against. No colour is
    /// written and a material that answers no shadow subview is left out rather than drawn with the
    /// wrong pass. The samplers are bound all the same: the pass a cutout material answers with
    /// reads its color map and discards under the clip, so a leaf that binds nothing casts its whole
    /// card.
    fn shadow(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let Some((into, _)) = self.buffers.shadow() else {
            return Ok(());
        };
        let size = self.buffers.shadow_size();
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.draw_buffers(&[glow::NONE]);
            gl.clear_color(1.0, 1.0, 1.0, 1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            // Every face casts, whichever way it points, and the depth is pushed back instead to
            // keep a surface off its own shadow. Culling the side that faces the sun costs a
            // one-sided shell - a pane of glass, a roof - its shadow entirely, that side being the
            // whole of it.
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::POLYGON_OFFSET_FILL);
        }
        for split in 0..program::SPLITS {
            let (view, projection) = program::shadow_camera(scene.light, scene.view, scene.projection, scene.reach, split);
            let sun = program::Scene {
                view,
                projection,
                split,
                ..scene.clone()
            };
            let offsets = self.windows(gl, frame, &sun, false)?;
            let instances = self.shadow_instances.ok_or("no shadow instance buffer")?;
            unsafe {
                let (column, row) = (
                    (split % program::ATLAS_COLUMNS) as i32,
                    (split / program::ATLAS_COLUMNS) as i32,
                );
                gl.viewport(column * size, row * size, size, size);
                gl.polygon_offset(program::SHADOW_SLOPE, program::shadow_push(scene.reach, split));
            }
            for (batch, (offset, windows, window)) in frame.batches.iter().zip(&offsets) {
                let meshes: Vec<i32> = match self
                    .models
                    .get(batch.model)
                    .and_then(Option::as_ref)
                    .and_then(|model| model.levels.get(batch.level))
                {
                    Some(level) => level.meshes.iter().map(|mesh| mesh.count).collect(),
                    None => continue,
                };
                for (mesh, (indices, surface)) in meshes.iter().zip(&batch.surfaces).enumerate() {
                    if surface.hidden {
                        continue;
                    }
                    let Some(shaded) = surface.shaded.as_ref() else {
                        continue;
                    };
                    let Some(held) = shaded.shadow.as_ref() else {
                        continue;
                    };
                    let program =
                        deferred::link(
                            gl,
                            &mut self.programs,
                            (surface.material, surface.waving, true, SUN),
                            held,
                        )?;
                    unsafe { gl.use_program(Some(program)) };
                    if held.batch() > 1 {
                        self.buffers.bind(gl, program, held, &sun, &[])?;
                    }
                    self.textures(gl, painter, program, held, shaded, surface.material)?;
                    let slot = held
                        .buffers
                        .iter()
                        .position(|buffer| buffer.instances() > 1)
                        .unwrap_or(0) as u32;
                    let Some(array) = self.array(gl, batch, mesh, &held.attributes)? else {
                        continue;
                    };
                    unsafe {
                        gl.bind_vertex_array(Some(array));
                        let count = held.batch() as i32;
                        // The batch's windows were laid out for its buffer pass, and a page or
                        // subview of the same material reading no instancing buffer takes none of
                        // them.
                        let taken = match count > 1 {
                            true => *windows,
                            false => 0,
                        };
                        for at in 0..taken {
                            gl.bind_buffer_range(
                                glow::UNIFORM_BUFFER,
                                slot,
                                Some(instances),
                                offset + at * aligned(*window, self.alignment),
                                *window,
                            );
                            let drawn = (batch.casts as i32 - at * count).min(count);
                            gl.draw_elements_instanced(
                                glow::TRIANGLES,
                                *indices,
                                glow::UNSIGNED_SHORT,
                                0,
                                drawn,
                            );
                        }
                        gl.bind_vertex_array(None);
                    }
                }
            }
        }
        unsafe {
            gl.disable(glow::POLYGON_OFFSET_FILL);
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
        }
        Ok(())
    }

    /// Every surface that lights itself, drawn twice: into a buffer of its own and then over the
    /// frame the composite left.
    ///
    /// The fill stands apart from the G-buffer because what these surfaces read back is the scene
    /// behind them: the frame the composite left, the view position, and the depth between the two.
    /// The G-buffer holds none of that the moment one of them writes into it.
    fn blended(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
        offsets: &[(i32, i32, i32)],
        lighting: &Lighting,
    ) -> Result<(), String> {
        let (mut fills, mut resolves) = (false, false);
        for shaded in frame
            .batches
            .iter()
            .flat_map(|batch| &batch.surfaces)
            .filter_map(|surface| surface.shaded.as_ref())
        {
            fills |= filled(shaded).is_some();
            resolves |= shaded.resolve.is_some();
        }
        if !fills && !resolves {
            return Ok(());
        }
        let size = self.buffers.size();
        if fills {
            self.buffers.sheer(gl)?;
            unsafe {
                gl.viewport(0, 0, size.0, size.1);
                gl.color_mask(true, true, true, true);
            }
            self.leg(gl, painter, frame, scene, offsets, true, false)?;
            self.buffers.relight(gl, lighting, scene, &frame.lamps)?;
        }
        // The reflection chain took its own copy before it ran and laid its answer back over the
        // frame afterward, so what stands behind a surface here is neither of those two.
        self.buffers.keep(gl)?;
        // Tested against a copy of the depth rather than the depth itself: a surface here also
        // samples it, and the live one is the framebuffer's own attachment.
        self.buffers.cut(gl)?;
        let into = self.buffers.bare().ok_or("no lit frame")?;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, size.0, size.1);
            gl.color_mask(true, true, true, true);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_mask(false);
        }
        self.mirror_water(gl, frame, scene)?;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, size.0, size.1);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_mask(false);
        }
        self.leg(gl, painter, frame, scene, offsets, false, false)?;
        unsafe {
            gl.depth_func(glow::LESS);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
        }
        Ok(())
    }

    /// The two members of water's own reflection chain that are drawn over the water itself: the
    /// mask, which stamps a stencil wherever the frame covers water, and the march, which walks the
    /// depth pyramid for what each of those pixels reflects. Everything past them is a quad and
    /// belongs to the graph.
    ///
    /// Its own projection rather than the frame's, which is what the camera buffer these read
    /// already holds where nothing named its fields: the near plane at the ordering the pyramid is
    /// stored in, and the second row negated. That negation turns the winding inside out, so the
    /// side culled is the other one, and it puts the answer the way round water addresses this map.
    fn mirror_water(
        &mut self,
        gl: &glow::Context,
        frame: &Frame,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let Some(chain) = frame.water_mirror.clone() else {
            return Ok(());
        };
        if !frame
            .batches
            .iter()
            .flat_map(|batch| &batch.surfaces)
            .filter_map(|surface| surface.shaded.as_ref())
            .any(watery)
        {
            return Ok(());
        }
        let watering = self.buffers.watering(gl, scene)?;
        // Every member of the chain reads the texel of what it is drawing into out of the same
        // buffer, which is the one the two below fill.
        let scene = &program::Scene {
            reflect: program::Reflect {
                level: 0,
                texel: glam::Vec2::new(
                    1.0 / watering.size.0 as f32,
                    1.0 / watering.size.1 as f32,
                ),
            },
            ..scene.clone()
        };
        // Both faces. The game culls the back one, which it can only do because it knows which way
        // its own water is wound; drawn under a projection whose second row is negated the winding
        // is the other way round, and culling the wrong side leaves the whole chain empty.
        unsafe { gl.disable(glow::CULL_FACE) };
        for (at, member) in [&chain.mask, &chain.march].into_iter().enumerate() {
            let program = deferred::link(gl, &mut self.water_programs, at, member)?;
            unsafe {
                gl.use_program(Some(program));
                gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
                match at {
                    // The mask writes no color at all: what it leaves behind is the stencil the
                    // march and every quad past it are cut to.
                    0 => {
                        gl.color_mask(false, false, false, false);
                        gl.stencil_mask(0xff);
                        gl.stencil_func(glow::ALWAYS, program::WATER_MIRROR_STENCIL, 0xff);
                        gl.stencil_op(glow::KEEP, glow::KEEP, glow::REPLACE);
                    }
                    _ => {
                        gl.color_mask(true, true, true, true);
                        gl.stencil_mask(0);
                        gl.stencil_func(glow::EQUAL, program::WATER_MIRROR_STENCIL, 0xff);
                        gl.stencil_op(glow::KEEP, glow::KEEP, glow::KEEP);
                    }
                }
            }
            for (unit, texture) in member.textures.iter().enumerate() {
                let bound = match texture.name.as_str() {
                    program::REFLECTION_DEPTH => watering.depth,
                    program::REFLECTION_FRAME => watering.frame,
                    _ => self.buffers.engine(gl, texture.id)?,
                };
                deferred::bind(
                    gl,
                    program,
                    &texture.name,
                    unit as u32,
                    bound,
                    deferred::target(texture.kind),
                    0.0,
                );
            }
            for batch in &frame.batches {
                let meshes: Vec<i32> = match self
                    .models
                    .get(batch.model)
                    .and_then(Option::as_ref)
                    .and_then(|model| model.levels.get(batch.level))
                {
                    Some(level) => level.meshes.iter().map(|mesh| mesh.count).collect(),
                    None => continue,
                };
                for (mesh, (indices, surface)) in meshes.iter().zip(&batch.surfaces).enumerate() {
                    if surface.hidden || !surface.shaded.as_ref().is_some_and(watery) {
                        continue;
                    }
                    let Some(array) = self.array(gl, batch, mesh, &member.attributes)? else {
                        continue;
                    };
                    for instance in &batch.instances {
                        let held = program::Scene {
                            model: instance.transform,
                            ..scene.clone()
                        };
                        self.buffers.bind(gl, program, member, &held, &[])?;
                        unsafe {
                            gl.bind_vertex_array(Some(array));
                            gl.draw_elements(glow::TRIANGLES, *indices, glow::UNSIGNED_SHORT, 0);
                            gl.bind_vertex_array(None);
                        }
                    }
                }
            }
        }
        unsafe {
            gl.color_mask(true, true, true, true);
        }
        self.buffers.water_mirror(gl, &chain, scene)
    }

    /// Every placed effect, one draw per unique file no matter how many placements share it. Tested
    /// against the depth the opaque and blended passes left standing, and a soft-particle
    /// apricot_model variant also samples a copy of it for a fade against what stands behind a
    /// particle. The copy rather than the live attachment: reading and writing the same depth in
    /// one draw is the feedback loop `blended()` once fell into.
    fn effects(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
    ) -> Result<(), String> {
        if frame.effects.is_empty() {
            return Ok(());
        }
        let into = self.buffers.lit().ok_or("no lit frame")?;
        let size = self.buffers.size();
        // Refreshed here rather than relied on from `blended()`: a frame with no blended surface
        // never runs that pass, and this copy has to be standing regardless.
        self.buffers.cut(gl)?;
        let depth = self.buffers.cutoff();
        let effect_scene = avfxprogram::Scene {
            view: scene.view,
            projection: scene.projection,
            size: (size.0 as f32, size.1 as f32),
            light: scene.light,
            diffuse: scene.diffuse,
            ..avfxprogram::Scene::default()
        };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, size.0, size.1);
            gl.color_mask(true, true, true, true);
        }
        for effect in &frame.effects {
            let Some(particles) = self.effects.get_mut(&effect.path) else {
                continue;
            };
            let held = avfxgpu::Frame {
                scene: avfxprogram::Scene {
                    fade_range: effect.fade_range,
                    ..effect_scene
                },
                batches: effect.batches.clone(),
                packages: frame.effect_packages.clone(),
                tested: true,
                depth,
            };
            particles.draw(gl, painter, &held);
        }
        unsafe {
            gl.depth_func(glow::LESS);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
        }
        Ok(())
    }

    /// The half of a surface the buffer pass clipped away, drawn through a buffer of its own: the
    /// fringe of a strand of hair the opaque half never draws, lit again and blended over the frame
    /// against the depth that half settled.
    fn sheer(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
        offsets: &[(i32, i32, i32)],
        lighting: &Lighting,
    ) -> Result<(), String> {
        let any = frame
            .batches
            .iter()
            .flat_map(|batch| &batch.surfaces)
            .filter_map(|surface| surface.shaded.as_ref())
            .any(|shaded| shaded.sheer.is_some());
        if !any {
            return Ok(());
        }
        let size = self.buffers.size();
        self.buffers.sheer(gl)?;
        unsafe {
            gl.viewport(0, 0, size.0, size.1);
            gl.color_mask(true, true, true, true);
        }
        self.leg(gl, painter, frame, scene, offsets, true, true)?;
        self.buffers.relight(gl, lighting, scene, &frame.lamps)?;
        self.buffers.cut(gl)?;
        let into = self.buffers.bare().ok_or("no lit frame")?;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, size.0, size.1);
            gl.color_mask(true, true, true, true);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_mask(false);
        }
        self.leg(gl, painter, frame, scene, offsets, false, true)?;
        unsafe {
            gl.depth_func(glow::LESS);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
        }
        Ok(())
    }

    /// One leg of that: the buffer each of them fills for itself, or what each answers into the
    /// frame with. A fringe leg is the same shape over the pair a hair-like material clips away
    /// instead: its own buffer, and what blends that over the frame the opaque half left.
    #[allow(clippy::too_many_arguments)]
    fn leg(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
        offsets: &[(i32, i32, i32)],
        filling: bool,
        fringe: bool,
    ) -> Result<(), String> {
        let instances = self.instances.ok_or("no instance buffer")?;
        for (batch, (offset, windows, window)) in frame.batches.iter().zip(offsets) {
            let meshes: Vec<i32> = match self
                .models
                .get(batch.model)
                .and_then(Option::as_ref)
                .and_then(|model| model.levels.get(batch.level))
            {
                Some(level) => level.meshes.iter().map(|mesh| mesh.count).collect(),
                None => continue,
            };
            for (mesh, (indices, surface)) in meshes.iter().zip(&batch.surfaces).enumerate() {
                if surface.hidden {
                    continue;
                }
                let Some(shaded) = &surface.shaded else {
                    continue;
                };
                let held = match (filling, fringe) {
                    (true, false) => filled(shaded),
                    (false, false) => shaded.resolve.as_ref(),
                    (true, true) => shaded.sheer.as_ref().map(|(buffer, _)| buffer),
                    (false, true) => shaded.sheer.as_ref().map(|(_, resolve)| resolve),
                };
                let Some(held) = held else {
                    continue;
                };
                let program = deferred::link(
                    gl,
                    &mut self.programs,
                    (
                        surface.material,
                        surface.waving,
                        false,
                        match (filling, fringe) {
                            (true, false) => 0,
                            (false, false) => LIT,
                            (true, true) => SHEER_BUFFER,
                            (false, true) => SHEER_RESOLVE,
                        },
                    ),
                    held,
                )?;
                // Each is tested against the depth the opaque passes settled rather than against a
                // fill of its own, which stands in a buffer beside them: a shaft of light adds what
                // it carries and everything else blends in by its own coverage. The frame's alpha
                // is left as it stands, since that channel holds the share of a pixel the composite
                // counted as glare rather than an opacity.
                let blend = match held.pass {
                    program::Pass::Shaft => (glow::ONE, glow::ONE),
                    _ => (glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA),
                };
                unsafe {
                    gl.use_program(Some(program));
                    gl.depth_func(glow::LESS);
                    match filling {
                        // Cut to what this buffer holds, which is one channel short of the
                        // opaque one.
                        true => {
                            let written = deferred::written(held);
                            gl.draw_buffers(&written[..written.len().min(deferred::SHEER)]);
                            gl.depth_mask(true);
                            gl.disable(glow::BLEND);
                        }
                        false => {
                            gl.depth_mask(false);
                            match held.pass {
                                // The game's own PASS_WATER never blends: its pixel shader samples
                                // the frame behind it through g_SamplerRefractionMap and writes the
                                // finished color. The alpha write is disabled tightly around the
                                // draw itself, below, rather than here: a `?` between this point and
                                // the draw would leave it off for the rest of the frame.
                                program::Pass::Water => gl.disable(glow::BLEND),
                                _ => {
                                    gl.enable(glow::BLEND);
                                    gl.blend_func_separate(blend.0, blend.1, glow::ZERO, glow::ONE);
                                }
                            }
                        }
                    }
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
                self.textures(gl, painter, program, held, shaded, surface.material)?;
                let slot = held
                    .buffers
                    .iter()
                    .position(|buffer| buffer.instances() > 1)
                    .unwrap_or(0) as u32;
                let Some(array) = self.array(gl, batch, mesh, &held.attributes)? else {
                    continue;
                };
                let count = held.batch() as i32;
                // The batch's windows were laid out for its buffer pass, and a resolve reading no
                // instancing buffer of its own takes none of them: those draw one object at a time,
                // off the transform the scene carries.
                let taken = match count > 1 {
                    true => *windows,
                    false => 0,
                };
                // Alpha stays out of the write mask only for water's own resolve draw, and only
                // across it: nothing fallible runs between the two calls that set it.
                let resolving_water = !filling && held.pass == program::Pass::Water;
                unsafe {
                    if resolving_water {
                        gl.color_mask(true, true, true, false);
                    }
                    gl.bind_vertex_array(Some(array));
                    for at in 0..taken {
                        gl.bind_buffer_range(
                            glow::UNIFORM_BUFFER,
                            slot,
                            Some(instances),
                            offset + at * aligned(*window, self.alignment),
                            *window,
                        );
                        let drawn = (batch.instances.len() as i32 - at * count).min(count);
                        gl.draw_elements_instanced(
                            glow::TRIANGLES,
                            *indices,
                            glow::UNSIGNED_SHORT,
                            0,
                            drawn,
                        );
                    }
                    gl.bind_vertex_array(None);
                    if resolving_water {
                        gl.color_mask(true, true, true, true);
                    }
                }
                if taken == 0 {
                    for instance in &batch.instances {
                        let held_scene = program::Scene {
                            model: instance.transform,
                            ..scene.clone()
                        };
                        self.buffers.bind(gl, program, held, &held_scene, &[])?;
                        unsafe {
                            if resolving_water {
                                gl.color_mask(true, true, true, false);
                            }
                            gl.bind_vertex_array(Some(array));
                            gl.draw_elements(glow::TRIANGLES, *indices, glow::UNSIGNED_SHORT, 0);
                            gl.bind_vertex_array(None);
                            if resolving_water {
                                gl.color_mask(true, true, true, true);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The zone's grass, over the geometry the scene baked per grid. The albedo reading goes down
    /// first and settles the depth; the second is tested against exactly what it left, since it
    /// samples nothing and would otherwise paint the whole blade, cut-away and all.
    fn grass(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
        page: usize,
    ) -> Result<(), String> {
        let Some(grass) = frame.grass.as_ref() else {
            return Ok(());
        };
        for (normal, held) in [
            (false, grass.buffer.get(page)),
            (true, grass.normal.get(page)),
        ] {
            let Some(held) = held.filter(|held| !held.targets.is_empty()) else {
                continue;
            };
            let program = deferred::link(gl, &mut self.programs, (TURF, false, normal, page), held)?;
            let supplied = held
                .textures
                .iter()
                .map(|texture| self.buffers.engine(gl, texture.id))
                .collect::<Result<Vec<_>, _>>()?;
            unsafe {
                gl.use_program(Some(program));
                gl.disable(glow::CULL_FACE);
                gl.enable(glow::DEPTH_TEST);
                gl.depth_mask(!normal);
                gl.depth_func(match normal {
                    true => glow::EQUAL,
                    false => glow::LEQUAL,
                });
                gl.color_mask(true, true, true, true);
                gl.draw_buffers(&deferred::written(held));
            }
            for blades in &frame.blades {
                let Some(turf) = self.turf.get_mut(&blades.turf) else {
                    continue;
                };
                let count = turf.count;
                let array = turf.array(gl, &held.attributes)?;
                let held_scene = program::Scene {
                    model: glam::Mat4::from_translation(blades.origin),
                    ..scene.clone()
                };
                self.buffers.bind(gl, program, held, &held_scene, &[])?;
                for (at, texture) in held.textures.iter().enumerate() {
                    let bound = match texture.id == COLOR_MAP {
                        true => painter.texture(blades.color_map),
                        false => None,
                    };
                    deferred::bind(
                        gl,
                        program,
                        &texture.name,
                        at as u32,
                        bound.unwrap_or(supplied[at]),
                        deferred::target(texture.kind),
                        0.0,
                    );
                }
                unsafe {
                    gl.bind_vertex_array(Some(array));
                    gl.draw_elements(glow::TRIANGLES, count, glow::UNSIGNED_INT, 0);
                    gl.bind_vertex_array(None);
                }
            }
        }
        unsafe {
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
        }
        Ok(())
    }

    /// Every character standing in the scene: with no lighting, what each fills the G-buffer with,
    /// and with one, what each answers into the frame the lighting left.
    ///
    /// Each carries its own place in the world and the colours it was made with, which are the two
    /// things the scene constants hold per character rather than per frame.
    ///
    /// A character whose own draw fails is skipped rather than failing the frame: the renderer
    /// takes a failure as final and stops drawing anything at all, and one material a driver
    /// rejects would then blank the whole zone.
    fn cast(
        &mut self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        frame: &Frame,
        scene: &program::Scene,
        lighting: Option<&Lighting>,
    ) {
        for cast in &frame.casts {
            let held = program::Scene {
                model: cast.model,
                customize: cast.customize,
                ..scene.clone()
            };
            let mut model = cast.gpu.lock().unwrap();
            let drawn = match lighting {
                None => model.fill(
                    gl,
                    painter,
                    (&cast.surfaces, &cast.joints),
                    &mut self.buffers,
                    &held,
                ),
                Some(lighting) => model.over(
                    gl,
                    painter,
                    &cast.surfaces,
                    &mut self.buffers,
                    lighting,
                    &frame.lamps,
                    &held,
                ),
            };
            if let Err(why) = drawn {
                log::error!("assets/layer: character: {why}");
            }
        }
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
        // Only the callback knows how many pixels the widget really covers, and a screen-wide pass
        // has nothing else to turn a fragment into a texel with.
        let scene = program::Scene {
            size: (size.0 as f32, size.1 as f32),
            ..frame.scene.clone()
        };
        let offsets = self.windows(gl, frame, &scene, true)?;
        let instances = self.instances.ok_or("no instance buffer")?;
        self.shadow(gl, painter, frame, &scene)?;

        for page in 0..self.buffers.pages() {
            self.buffers.open(gl, page);
            // What the last draw left the context set to. Thousands of surfaces run through the
            // same handful of readings, and a draw only has to set what the one before it left
            // wrong. Kept per page, since the draw buffers belong to the framebuffer that page
            // opened.
            let mut standing: Option<(glow::Program, bool, Vec<u32>, bool)> = None;
            for depth in [true, false] {
                for (batch, (offset, windows, window)) in frame.batches.iter().zip(&offsets) {
                    // Taken by value first: the draw wants the frame's own buffers mutably, and
                    // the models would still be borrowed.
                    let meshes: Vec<i32> = match self
                        .models
                        .get(batch.model)
                        .and_then(Option::as_ref)
                        .and_then(|model| model.levels.get(batch.level))
                    {
                        Some(level) => level.meshes.iter().map(|mesh| mesh.count).collect(),
                        None => continue,
                    };
                    for (mesh, (indices, surface)) in
                        meshes.iter().zip(&batch.surfaces).enumerate()
                    {
                        if surface.hidden {
                            continue;
                        }
                        let Some(shaded) = &surface.shaded else {
                            continue;
                        };
                        let held = match depth {
                            true => shaded.depth.as_ref(),
                            false => shaded.buffer.get(page),
                        };
                        // A surface with no opaque pass fills a buffer of its own, after the
                        // lighting rather than here.
                        let Some(held) = held.filter(|held| {
                            depth
                                || (!held.targets.is_empty()
                                    && held.pass != program::Pass::Blended)
                        }) else {
                            continue;
                        };
                        let program = deferred::link(
                            gl,
                            &mut self.programs,
                            (surface.material, surface.waving, depth, page),
                            held,
                        )?;
                        // A material with no depth pass writes its own, since the depth buffer is
                        // what says which pixels the frame covered.
                        let wanted = (
                            program,
                            depth || shaded.depth.is_none(),
                            deferred::written(held),
                            surface.cull,
                        );
                        if standing.as_ref() != Some(&wanted) {
                            let (program, writes, targets, cull) = &wanted;
                            unsafe {
                                gl.use_program(Some(*program));
                                gl.depth_mask(*writes);
                                gl.color_mask(!depth, !depth, !depth, !depth);
                                gl.draw_buffers(targets);
                                match cull {
                                    true => {
                                        gl.enable(glow::CULL_FACE);
                                        gl.cull_face(glow::BACK);
                                        gl.front_face(glow::CCW);
                                    }
                                    false => gl.disable(glow::CULL_FACE),
                                }
                            }
                            standing = Some(wanted);
                        }
                        if held.batch() > 1 {
                            self.buffers.bind(gl, program, held, &scene, &[])?;
                        }
                        self.textures(gl, painter, program, held, shaded, surface.material)?;
                        let slot = held
                            .buffers
                            .iter()
                            .position(|buffer| buffer.instances() > 1)
                            .unwrap_or(0) as u32;
                        let Some(array) = self.array(gl, batch, mesh, &held.attributes)? else {
                            continue;
                        };
                        let count = held.batch() as i32;
                        let taken = match count > 1 {
                            true => *windows,
                            false => 0,
                        };
                        unsafe {
                            gl.bind_vertex_array(Some(array));
                            for at in 0..taken {
                                gl.bind_buffer_range(
                                    glow::UNIFORM_BUFFER,
                                    slot,
                                    Some(instances),
                                    offset + at * aligned(*window, self.alignment),
                                    *window,
                                );
                                let drawn = (batch.instances.len() as i32 - at * count).min(count);
                                gl.draw_elements_instanced(
                                    glow::TRIANGLES,
                                    *indices,
                                    glow::UNSIGNED_SHORT,
                                    0,
                                    drawn,
                                );
                            }
                            gl.bind_vertex_array(None);
                        }
                        // A package that reads no instancing buffer draws one object at a time,
                        // off the transform the scene carries.
                        if taken == 0 {
                            for instance in &batch.instances {
                                let held_scene = program::Scene {
                                    model: instance.transform,
                                    ..scene.clone()
                                };
                                self.buffers.bind(gl, program, held, &held_scene, &[])?;
                                unsafe {
                                    gl.bind_vertex_array(Some(array));
                                    gl.draw_elements(
                                        glow::TRIANGLES,
                                        *indices,
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
            self.grass(gl, painter, frame, &scene, page)?;
        }
        self.cast(gl, painter, frame, &scene, None);

        if let Some(lighting) = frame.lighting.as_ref() {
            // Before anything reads it: every lighting pass and the composite take the occlusion as
            // a weight on what they work out.
            match frame.occlusion.as_ref() {
                Some(held) => self.buffers.occlude(gl, held, &scene)?,
                None => self.buffers.unocclude(),
            }
            // Before the lighting too: the shadowed variant reads the mask as a weight on what it
            // works out, so it has to be standing before the first light is resolved.
            match lighting.shadow.as_ref() {
                Some(held) => self.buffers.shade(gl, held, &scene)?,
                None => self.buffers.unshade(),
            }
            // And the cloud's own shadow beside it, which that same variant reads as a second
            // weight. The sheet is drawn from where the sun is rather than from the camera, and it
            // stands where the sky's own draw of it will.
            match frame.cloud_shadow.as_ref() {
                Some((sheet, blur)) => {
                    let (view, projection) =
                        program::Cloud::shadow_camera(scene.light, scene.view);
                    let overhead = program::Scene {
                        model: program::Cloud::placement(
                            program::Pass::CloudSheet,
                            scene.view.inverse().w_axis.truncate(),
                        ),
                        view,
                        projection,
                        ..scene.clone()
                    };
                    self.buffers.cloud_shadow(gl, sheet, blur, &overhead)?;
                }
                None => self.buffers.unclouded(),
            }
            self.buffers.resolve(gl, lighting, &scene, &frame.lamps)?;
            // Before the exposure, which reads the whole frame: a black hole where the sky belongs
            // measures as a far darker scene than it is.
            if let Some(skybox) = frame.skybox.as_ref() {
                self.buffers.sky(gl, skybox, &scene)?;
            }
            // What the glare chain spreads, kept over the sky and under everything below: each of
            // those writes an alpha of its own over the share of a pixel the composite marked as
            // glare, and the game keeps its own copy at this point for the same reason.
            if frame.glare.is_some() {
                self.buffers.source(gl)?;
            }
            if frame.skybox.is_some() {
                // Over the sky and under the clouds, which is where a real frame draws it.
                if let Some(held) = frame.sunlight.as_ref() {
                    self.buffers.sun(gl, held, &scene)?;
                }
                if let Some(held) = frame.moonlight.as_ref() {
                    self.buffers.moon(gl, held, &scene)?;
                }
                if let Some(held) = frame.starlight.as_ref() {
                    let dome = program::Scene {
                        model: program::Star::placement(scene.view.inverse().w_axis.truncate()),
                        ..scene.clone()
                    };
                    self.buffers.stars(gl, held, &dome)?;
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
                // Over the frame the sky and the clouds left and before the water reads it, which
                // is where the game runs it.
                if let Some(reflection) = frame.reflection.as_ref() {
                    self.buffers.mirror(gl, reflection, &scene)?;
                }
                // After both, which is what it fades the far distance toward, and before the
                // exposure, which measures the frame the fog leaves rather than the one under it.
                self.blended(gl, painter, frame, &scene, &offsets, lighting)?;
                if let Some(haze) = frame.haze.as_ref() {
                    self.buffers.fog(gl, haze, &scene)?;
                }
                // After the fog: the game never fogs a placed effect, so anything drawn here lands
                // untouched by it rather than blended toward it by however much of the frame the fog
                // pass covered.
                self.effects(gl, painter, frame, &scene)?;
            }
            // Over the sky, the water and the fog: a fringe pixel is one the opaque pass left
            // uncovered, and drawing it any earlier leaves the sky free to paint over it.
            self.sheer(gl, painter, frame, &scene, &offsets, lighting)?;
            self.cast(gl, painter, frame, &scene, Some(lighting));
            // Before the exposure, since a halo belongs to the frame the lighting left rather than
            // to what a curve made of it.
            if let Some(glare) = frame.glare.as_ref() {
                self.buffers.glare(gl, glare, &scene)?;
            }
            if let Some(exposure) = frame.exposure.as_ref() {
                self.buffers.expose(gl, exposure, &scene)?;
            }
            if let Some(smoothing) = frame.smoothing.as_ref() {
                self.buffers.antialias(gl, smoothing, &scene)?;
            }
            // Last, over the graded frame, which is where the game draws it.
            if let Some(vignette) = frame.vignette.as_ref() {
                self.buffers.vignette(gl, vignette, &scene)?;
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
                .chain(std::mem::take(&mut self.turf).into_values().flat_map(Turf::dead))
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

/// The buffer a surface fills for itself, where its package has no opaque pass and fills one
/// through a semitransparent pass instead.
fn filled(shaded: &Shaded) -> Option<&Arc<program::Program>> {
    shaded
        .buffer
        .first()
        .filter(|held| held.pass == program::Pass::Blended)
}

/// Whether a surface is water, which is the one thing the chain that fills its reflection draws.
fn watery(shaded: &Shaded) -> bool {
    shaded
        .resolve
        .as_ref()
        .is_some_and(|held| held.pass == program::Pass::Water)
}

/// The next offset a uniform buffer will let a window start on.
fn aligned(bytes: i32, alignment: i32) -> i32 {
    let held = alignment.max(1);
    (bytes + held - 1) / held * held
}

/// The array a set of attributes is pointed on, pointing one the first time a pass asks for it. The
/// upload leaves an array behind with nothing pointed on it, since the indices had to be bound under
/// one, and the first set asked for takes that rather than building another.
fn pointed(
    gl: &glow::Context,
    arrays: &mut Vec<(Vec<program::Attribute>, glow::VertexArray)>,
    geometry: (glow::Buffer, glow::Buffer),
    attributes: &[program::Attribute],
    onto: fn(&glow::Context, &program::Attribute),
) -> Result<glow::VertexArray, String> {
    if let Some((_, layout)) = arrays.iter().find(|(held, _)| held.as_slice() == attributes) {
        return Ok(*layout);
    }
    let layout = match arrays.iter().position(|(held, _)| held.is_empty()) {
        Some(at) => {
            arrays[at].0 = attributes.to_vec();
            arrays[at].1
        }
        None => {
            let layout = unsafe { gl.create_vertex_array()? };
            arrays.push((attributes.to_vec(), layout));
            layout
        }
    };
    unsafe {
        gl.bind_vertex_array(Some(layout));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(geometry.0));
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(geometry.1));
        for held in attributes {
            onto(gl, held);
        }
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
    }
    Ok(layout)
}

/// One of the grass semantics pointed at the corner it sits in.
fn corner(gl: &glow::Context, held: &program::Attribute) {
    let Some((_, lanes, offset, kind)) = CORNERS.iter().find(|(field, ..)| *field == held.field)
    else {
        return;
    };
    let stride = size_of::<Corner>() as i32;
    unsafe {
        gl.enable_vertex_attrib_array(held.location);
        match held.components {
            program::Components::Float => {
                gl.vertex_attrib_pointer_f32(held.location, *lanes, *kind, false, stride, *offset)
            }
            _ => gl.vertex_attrib_pointer_i32(held.location, *lanes, *kind, stride, *offset),
        }
    }
}

/// One grid's blades and the quads over them, under a vertex array of their own for the same reason
/// a mesh has one.
fn upload_turf(
    gl: &glow::Context,
    corners: &[Corner],
    indices: &[u32],
) -> Result<Turf, String> {
    unsafe {
        let layout = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(layout));

        let vertices = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vertices));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(corners),
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
        Ok(Turf {
            vertices,
            indices: drawn,
            count: indices.len() as i32,
            arrays: vec![(Vec::new(), layout)],
        })
    }
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

/// One mesh's buffers, under a vertex array of its own. The array is not an optimization: egui
/// leaves its own bound while a callback runs, so binding the indices without one would hand egui's
/// layout this mesh's.
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
            vertices: held,
            indices: drawn,
            count: indices.len() as i32,
            arrays: vec![(Vec::new(), layout)],
        })
    }
}
