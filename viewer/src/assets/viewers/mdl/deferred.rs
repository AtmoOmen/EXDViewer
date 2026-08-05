//! The frame the game's own shaders draw into, and the passes that resolve it.
//!
//! A G-buffer written a page of targets at a time, the buffers the lighting passes fill from it, and
//! the composite that turns those into a frame. One model and a whole zone want the same thing here,
//! so this is what they share; what differs is only the draw list that fills the G-buffer.

use std::collections::BTreeMap;

use glow::HasContext;

use super::program;

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

/// Channels of the G-buffer, which is what its pages add up to however many a context can write at
/// once, and the channel past the last of them: the frame the composite resolved.
pub const TARGETS: usize = 5;
pub const LIT: usize = TARGETS;

/// The one structured buffer that is not a joint palette.
pub const TYPES: &str = "g_ShaderTypeParameter";

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

/// The corners and triangles of the volume a light covers, which its own vertex shader clamps to the
/// extents the zone clips it against and then projects. Not a screen-wide pass: a light only reaches
/// the pixels its volume covers.
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

const PRESENT_VERTEX: &str = include_str!("present.vert");
const PRESENT_FRAGMENT: &str = include_str!("present.frag");

/// GL objects with nothing left to draw them, waiting for a context to delete them under. A viewer
/// is dropped between frames, where there is no context, so its objects outlive it by one callback.
static GRAVEYARD: std::sync::OnceLock<std::sync::Mutex<Vec<Dead>>> = std::sync::OnceLock::new();

pub enum Dead {
    Layout(glow::VertexArray),
    Buffer(glow::Buffer),
    Texture(glow::Texture),
    Program(glow::Program),
    Renderbuffer(glow::Renderbuffer),
    Frame(glow::Framebuffer),
}

pub fn graveyard() -> &'static std::sync::Mutex<Vec<Dead>> {
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
    pub position: std::sync::Arc<program::Program>,
    pub directional: std::sync::Arc<program::Program>,
    pub point: std::sync::Arc<program::Program>,
    pub composite: std::sync::Arc<program::Program>,
}

/// A linked pair of the game's own shaders, and the source it was built from so a change rebuilds
/// it rather than a stale program drawing on.
pub struct Linked {
    source: String,
    pub program: glow::Program,
}

/// Everything the graph draws into, and the passes past the G-buffer.
#[derive(Default)]
pub struct Buffers {
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
    types: Option<glow::Texture>,
    stand_in: Option<glow::Texture>,
    unoccluded: Option<glow::Texture>,
    screen: Option<(glow::VertexArray, glow::Buffer)>,
    volume: Option<(glow::VertexArray, glow::Buffer, glow::Buffer)>,
    resolvers: BTreeMap<usize, Linked>,
    present: Option<glow::Program>,
    blocks: Vec<glow::Buffer>,
}

impl Buffers {
    pub fn size(&self) -> (i32, i32) {
        self.size
    }

    /// How much of the G-buffer one pass can write. Four until a frame has asked the context, since
    /// that is what a context is promised.
    pub fn attachments(&self) -> usize {
        match self.attachments {
            0 => 4,
            held => held,
        }
    }

    pub fn pages(&self) -> usize {
        self.frames.len()
    }

    /// What a viewer puts on screen: one channel of the G-buffer, or the frame the composite
    /// resolved past the last of them.
    fn channel(&self, at: usize) -> Option<glow::Texture> {
        match at >= TARGETS {
            true => self.lit.map(|(_, texture)| texture),
            false => self.color.get(at).copied(),
        }
    }

    /// Draws one of those over the widget and leaves egui's own framebuffer bound behind it.
    ///
    /// A pass rather than a blit: the framebuffer a browser hands a callback is multisampled, and
    /// blitting into one of those is an error rather than a resolve. The depth buffer goes with it,
    /// since that is what says which pixels the frame covered and which are egui's to keep.
    pub fn show(
        &mut self,
        gl: &glow::Context,
        at: usize,
        into: Option<glow::Framebuffer>,
        viewport: (i32, i32, i32, i32),
    ) -> Result<(), String> {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, into);
            // The default framebuffer draws to the back buffer and one of its own draws to its
            // first attachment; naming the wrong one is an error rather than a no-op.
            match into {
                Some(_) => gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]),
                None => gl.draw_buffers(&[glow::BACK]),
            }
            gl.viewport(viewport.0, viewport.1, viewport.2, viewport.3);
            // Back on for the widget, which is drawn in the window's own coordinates, where that
            // clip rect means what it says.
            gl.enable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.depth_mask(false);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
        }
        let texture = self
            .channel(at)
            .ok_or_else(|| format!("the frame has no buffer {at}"))?;
        let depth = self.depth.ok_or("no depth buffer")?;
        let program = match self.present {
            Some(held) => held,
            None => {
                let held = build_pair(gl, PRESENT_VERTEX, PRESENT_FRAGMENT)?;
                self.present = Some(held);
                held
            }
        };
        let layout = self.screen(gl)?;
        unsafe {
            gl.use_program(Some(program));
            sampler(gl, program, "u_frame", 0, texture);
            sampler(gl, program, "u_depth", 1, depth);
            gl.bind_vertex_array(Some(layout));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
        }
        Ok(())
    }

    /// Every buffer of the graph, sized to what is being drawn into.
    ///
    /// The G-buffer is one framebuffer per page of its targets: a context is promised four draw
    /// buffers and a framebuffer no more color attachments than that, so five targets cannot all
    /// hang off one, and what a page cannot hold is written by a reading of its own.
    pub fn attach(&mut self, gl: &glow::Context, size: (i32, i32)) -> Result<(), String> {
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

    /// Clears one page of the G-buffer and points the draw buffers at every attachment it has.
    pub fn open(&self, gl: &glow::Context, page: usize) {
        let Some(held) = self.frames.get(page) else {
            return;
        };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(*held));
            // A clear only reaches the attachments the draw buffers name, and a framebuffer starts
            // out naming its first alone: without this the rest keep whatever the texture happened
            // to be created holding.
            let width = (TARGETS - page * self.attachments).min(self.attachments);
            let attachments: Vec<u32> = (0..width)
                .map(|at| glow::COLOR_ATTACHMENT0 + at as u32)
                .collect();
            gl.draw_buffers(&attachments);
            gl.viewport(0, 0, self.size.0, self.size.1);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            // egui leaves the scissor set to the widget's rect in the window's own coordinates, and
            // the frame is a buffer of its own that starts at nought: leaving it on clips the clear
            // and every draw after it to whatever part of the frame the rect happens to overlap.
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.color_mask(true, true, true, true);
            // Every page draws the same geometry against one depth buffer, so only the first of
            // them clears it: what a later page does not draw still covered the pixel.
            gl.clear(match page {
                0 => glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT,
                _ => glow::COLOR_BUFFER_BIT,
            });
        }
    }

    /// The screen-wide triangle's array, uploaded the first time something draws it.
    fn screen(&mut self, gl: &glow::Context) -> Result<glow::VertexArray, String> {
        if let Some((layout, _)) = self.screen {
            return Ok(layout);
        }
        let held = upload_screen(gl)?;
        self.screen = Some(held);
        Ok(held.0)
    }

    /// The one texture standing in for whatever a material binds nothing to.
    pub fn stand_in(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
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
    pub fn types(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.types {
            return Ok(held);
        }
        let held = dwords(gl, &program::shader_types())?;
        self.types = Some(held);
        Ok(held)
    }

    /// The buffers a screen-wide pass reads, by the name the package knows each by. One nothing here
    /// fills is the flat stand-in, which is what leaves its term out rather than crashing the draw.
    pub fn engine(&mut self, gl: &glow::Context, id: u32) -> Result<glow::Texture, String> {
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
    pub fn bind(
        &mut self,
        gl: &glow::Context,
        program: glow::Program,
        held: &program::Program,
        scene: &program::Scene,
        instances: &[program::Instance],
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
                let mut data = buffer.fill(scene, held.pass, instances);
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
        let program = match self.resolvers.get(&at) {
            Some(linked) if linked.source == source => linked.program,
            _ => {
                let built = build_pair(gl, &held.vertex, &held.fragment)?;
                if let Some(stale) = self.resolvers.insert(
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
            false => self.screen(gl)?,
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
        self.bind(gl, program, held, scene, &[])?;
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
    /// frame off the light. Every lamp adds to the same two buffers, so they start at nothing and
    /// each pass adds to what the one before it left.
    pub fn resolve(
        &mut self,
        gl: &glow::Context,
        lighting: &Lighting,
        scene: &program::Scene,
        lamps: &[program::Lamp],
    ) -> Result<(), String> {
        let (position, _) = self.position.ok_or("no view position")?;
        let (light, _) = self.light.ok_or("no light buffer")?;
        let (lit, _) = self.lit.ok_or("no lit frame")?;
        unsafe {
            // A screen-wide pass covers every pixel and reads the depth rather than testing against
            // it, so nothing here is depth tested and nothing writes depth.
            gl.disable(glow::DEPTH_TEST);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
        }
        self.pass(gl, 0, &lighting.position, position, scene, false)?;

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(light));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0, glow::COLOR_ATTACHMENT1]);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
        }
        self.pass(gl, 1, &lighting.directional, light, scene, false)?;
        // One face of the volume, not both. The pass adds what it computes to the buffer, so a box
        // shaded front and back would light every pixel it covers twice over. The far face is the
        // one kept, since it still covers the frame when the camera stands inside the light.
        unsafe {
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::FRONT);
            gl.front_face(glow::CCW);
        }
        // Every lamp is the same pass over a volume of its own, so the program is linked once and
        // only the buffer it reads is written again.
        for lamp in lamps {
            let held = program::Scene {
                lamp: *lamp,
                ..*scene
            };
            self.pass(gl, 2, &lighting.point, light, &held, true)?;
        }
        unsafe {
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
        };
        self.pass(gl, 3, &lighting.composite, lit, scene, false)
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.color.drain(..).map(Dead::Texture));
        dead.extend(
            [
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
        dead.extend(self.present.take().map(Dead::Program));
        dead.extend(
            std::mem::take(&mut self.resolvers)
                .into_values()
                .map(|held| Dead::Program(held.program)),
        );
    }
}

/// One linked pair, kept against the source it was built from so a change rebuilds it rather than a
/// stale program drawing on.
pub fn link<K: Ord>(
    gl: &glow::Context,
    into: &mut BTreeMap<K, Linked>,
    key: K,
    held: &program::Program,
) -> Result<glow::Program, String> {
    let source = format!("{}\n{}", held.vertex, held.fragment);
    if let Some(linked) = into.get(&key)
        && linked.source == source
    {
        return Ok(linked.program);
    }
    let built = build_pair(gl, &held.vertex, &held.fragment)?;
    if let Some(stale) = into.insert(
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

/// Point sampling and no wrap, which is what every buffer of the graph is read with: a shader
/// addresses texel centers and works out for itself what lies between them.
pub fn point(gl: &glow::Context) {
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
pub fn dwords(gl: &glow::Context, values: &[u32]) -> Result<glow::Texture, String> {
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
pub fn sampler(
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

/// The geometry the screen-wide passes and a light's volume are drawn from, in one array each so a
/// pass can draw either without rebinding anything else.
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

pub fn build_pair(
    gl: &glow::Context,
    vertex: &str,
    fragment: &str,
) -> Result<glow::Program, String> {
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
