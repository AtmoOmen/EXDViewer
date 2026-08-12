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

/// The frame as the composite left it, which is what a semitransparent pass blends over.
const FINAL_COLOR: u32 = 0x8ea9_df48;

/// What every member of the post chain calls the frame it reads.
const INPUT: u32 = 0x527d_95a1;

/// What the occlusion chain calls what it reads: the depth buffer and the G-buffer channel holding
/// the normal, then what each of its passes leaves for the next.
const DEPTH_PLANE: u32 = 0x70f6_bb1f;
const NORMAL_PLANE: u32 = 0x4a6b_06c7;
const DEPTH_NORMAL_Z: u32 = 0xe1fb_187c;
const GATHER_DEPTH: u32 = 0x5ee7_1209;
const GATHER_NORMAL_Z: u32 = 0x3d66_0475;

/// The G-buffer channel holding the surface normal. In the world rather than in front of the
/// camera: every pass that lights the frame brings it through the view matrix itself, and so does
/// the one that scales it down here.
const NORMAL_CHANNEL: usize = 0;

/// The ramp a placed light's falloff is read off, indexed by the square of how far the pixel stands
/// from it. The engine binds this rather than any material, and the flat stand-in leaves every light
/// at full strength out to the edge of its own volume, which is a hard circle.
pub const RAMP: (u32, &str, u32) = (
    ATTENUATION,
    "common/graphics/texture/-attenuation.tex",
    glow::LINEAR,
);

/// The textures the game's own shaders read that no material names, and how each is read between its
/// texels: the tiles a character's surface detail is taken from, the spheres its resolve pass shades
/// against, that ramp, the profiles the subsurface term is read off, and the ramps a cel-shaded
/// surface takes its whole light response off, a profile to a row and the cosine along the row.
///
/// The kernel is addressed at whole texels, a profile to a row and a Gaussian to a column, so
/// filtering it would answer with the mean of two profiles and of two Gaussians alike.
pub const ENGINE: [(u32, &str, u32); 6] = [
    (
        0x92f0_3e53,
        "chara/common/texture/tile_norm_array.tex",
        glow::LINEAR,
    ),
    (
        0x800b_e99b,
        "chara/common/texture/tile_orb_array.tex",
        glow::LINEAR,
    ),
    (
        0x3334_d3ca,
        "chara/common/texture/sphere_d_array.tex",
        glow::LINEAR,
    ),
    RAMP,
    (
        0x3b44_510e,
        "common/graphics/texture/-sss_kernel_ssst.tex",
        glow::NEAREST,
    ),
    (0x8b73_3c20, "chara/common/texture/-toon.tex", glow::LINEAR),
];

/// The table the frame is graded through, which the grading pass addresses by the color it found.
/// Three of these ship and nothing states which one binds; this is the one that answers a grey with
/// the grey it was given and departs from the identity least of the three.
pub const GRADING: (u32, &str, u32) = (
    0xabc0_472a,
    "common/graphics/texture/-output_lut_d.tex",
    glow::LINEAR,
);

/// Where the grading pass is linked, past the slots the lighting and the composite take.
const POST: usize = 5;

/// Channels of the G-buffer, which is what its pages add up to however many a context can write at
/// once, and the channel past the last of them: the frame the composite resolved.
pub const TARGETS: usize = 5;
pub const LIT: usize = TARGETS;

/// The channel the fur pass softens. It squares what it reads and takes the root of what it writes,
/// and the surface color is the one channel a drawing package gamma-encodes; the package asks for it
/// by a name every package that shades a surface gives the channel after it.
const FUR_CHANNEL: usize = 2;

/// The one structured buffer that is not a joint palette.
pub const TYPES: &str = "g_ShaderTypeParameter";

/// What a texture the material binds nothing to answers with.
const STAND_IN: [u8; 4] = [128, 128, 128, 255];

/// The tables the engine works out a frame at a time and no file holds, each at the value that
/// leaves the term it drives where it was. `g_SamplerToneMapLut` divides the resolved color, so the
/// flat stand-in would double every pixel; a fog weight of nought keeps the color it mixes toward
/// out of the frame entirely.
const NEUTRAL: [(u32, [u8; 4]); 2] = [
    (0x342f_2734, [255, 255, 255, 255]),
    (0x6e23_1669, [0, 0, 0, 0]),
];

/// What a buffer nothing here fills answers with where a lighting pass wants a weight: nothing
/// shadowed in the red the lighting reads, nothing faded in the alpha the composite reads.
const UNOCCLUDED: [u8; 4] = [255, 255, 255, 0];

/// What the composite takes a reflection against before a place has said what its sky looks like.
const UNREFLECTED: [u8; 4] = [128, 128, 128, 0];

/// Texels a face of the reflection cube takes. The sky it is built from is three harmonic rows,
/// which carry nothing finer than this.
const SKY_FACE: i32 = 16;

/// Which way a texel of a cube face looks, in the space the composite samples the cube in.
fn facing(face: usize, u: f32, v: f32) -> glam::Vec3 {
    match face {
        0 => glam::Vec3::new(1.0, -v, -u),
        1 => glam::Vec3::new(-1.0, -v, u),
        2 => glam::Vec3::new(u, 1.0, v),
        3 => glam::Vec3::new(u, -1.0, -v),
        4 => glam::Vec3::new(u, -v, 1.0),
        _ => glam::Vec3::new(-u, -v, -1.0),
    }
    .normalize()
}

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

/// What a pass of the graph covers: the whole frame, what one light reaches, or the whole frame
/// again where the fur pass reads the channel it answers into as the G-buffer left it.
#[derive(Clone, Copy)]
enum Over {
    Screen,
    Volume,
    Softening(glow::Texture),
    /// A member of the post chain reading what the one before it wrote rather than the frame the
    /// composite resolved.
    Reading(glow::Texture),
}

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
#[derive(Clone)]
pub struct Lighting {
    pub position: std::sync::Arc<program::Program>,
    pub directional: std::sync::Arc<program::Program>,
    pub point: std::sync::Arc<program::Program>,
    /// Absent until a zone's own spot package has arrived, and always where nothing places a spot.
    pub spot: Option<std::sync::Arc<program::Program>>,
    /// The same for fur, which only a surface whose own record states a length has any of.
    pub fur: Option<std::sync::Arc<program::Program>>,
    pub composite: std::sync::Arc<program::Program>,
}

/// The pair that smooths the frame's edges, in the order they run.
pub struct Smoothing {
    pub luma: std::sync::Arc<program::Program>,
    pub fxaa: std::sync::Arc<program::Program>,
}

/// The chain that works out how much of the sky reaches each pixel, in the order it runs.
pub struct Occlusion {
    pub scale: std::sync::Arc<program::Program>,
    pub gather: std::sync::Arc<program::Program>,
    pub occlude: std::sync::Arc<program::Program>,
}

/// A layered texture as the card takes one: its slices one after the next, in RGBA bytes.
pub struct Layered {
    pub size: (i32, i32),
    pub layers: i32,
    pub pixels: Vec<u8>,
    pub filter: u32,
    /// Whether the file addresses its slices as a third dimension rather than holding a stack of
    /// separate images. A sampler is declared over one or the other, and a draw only validates where
    /// the texture bound to its unit is of the declaration's own kind.
    pub volumetric: bool,
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
    /// What the composite left, kept apart from the frame a semitransparent pass writes: that pass
    /// reads the one and writes the other, and a texture cannot be both at once.
    resolved: Option<glow::Texture>,
    /// A framebuffer over the channel the fur pass softens, and that channel as the G-buffer left
    /// it: the pass walks a strand across its neighbours to answer for one pixel.
    fur: Option<(glow::Framebuffer, glow::Texture)>,
    /// The frame with each pixel's brightness in its alpha, which is what the smoothing pass reads
    /// its edges off. Filtered rather than point sampled, since that pass reads between texels along
    /// the edge it found.
    smoothed: Option<(glow::Framebuffer, glow::Texture)>,
    /// The occlusion chain's own buffers, at a fraction of the frame: the depth and normal it works
    /// from, the square of four of each its taps address, and how much sky reached the pixel. That
    /// last is filtered, since the lighting reads it back over the whole frame.
    scaled: Option<(glow::Framebuffer, glow::Texture)>,
    gathered: Option<(glow::Framebuffer, [glow::Texture; 2])>,
    occluded: Option<(glow::Framebuffer, glow::Texture)>,
    /// Whether that chain ran this frame. Every pass reads the flat stand-in until it has, and again
    /// from the frame the viewer stops asking for it.
    occluding: bool,
    size: (i32, i32),
    /// What the context allows, which is what decides how much of the G-buffer one pass can write.
    attachments: usize,
    types: Option<glow::Texture>,
    /// One texel of the same value per target a sampler can be declared over. A draw is rejected
    /// outright where what is bound to a unit is not of the declaration's own kind, so a plane
    /// cannot stand in for the rest.
    blanks: BTreeMap<u32, glow::Texture>,
    /// The textures the shaders read off the game's own files, by resource id, each with the target
    /// its file was taken onto the card at.
    arrays: BTreeMap<u32, (u32, glow::Texture)>,
    neutrals: BTreeMap<u32, glow::Texture>,
    unoccluded: Option<glow::Texture>,
    reflection: Option<glow::Texture>,
    /// The sky the reflection cube was built from, so a frame asking for the same one keeps it.
    sky: Option<([glam::Vec4; 3], f32)>,
    screen: Option<(glow::VertexArray, glow::Buffer)>,
    volume: Option<(glow::VertexArray, glow::Buffer, glow::Buffer)>,
    resolvers: BTreeMap<usize, Linked>,
    present: Option<glow::Program>,
    blocks: Vec<glow::Buffer>,
    /// Whether the graph has already brought the frame into the range a screen holds, which is what
    /// keeps the pass that puts it up from bending it a second time.
    toned: bool,
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

    /// Draws one of those over the widget and leaves egui's own framebuffer bound behind it. A raw
    /// channel is read as data rather than looked at, so only the frame the composite resolved is
    /// bent toward what a screen holds.
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
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            // The frame carries its own depth up with it, so whatever is drawn over the widget
            // afterwards can test against what it covered. The pixels this pass drops would keep
            // what an earlier frame left there, hence the clear, and a fragment writes no depth at
            // all with the test off, hence the test.
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::ALWAYS);
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
        }
        let texture = self
            .channel(at)
            .ok_or_else(|| format!("the frame has no buffer {at}"))?;
        let depth = self.depth.ok_or("no depth buffer")?;
        let program = self.presenter(gl)?;
        let layout = self.screen(gl)?;
        unsafe {
            gl.use_program(Some(program));
            sampler(gl, program, "u_frame", 0, texture);
            sampler(gl, program, "u_depth", 1, depth);
            if let Some(location) = gl.get_uniform_location(program, "u_tone") {
                gl.uniform_1_i32(Some(&location), i32::from(at >= TARGETS && !self.toned));
            }
            gl.bind_vertex_array(Some(layout));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
            gl.depth_mask(false);
            gl.disable(glow::DEPTH_TEST);
        }
        Ok(())
    }

    /// One channel of the frame read off the card, each component between nought and one.
    ///
    /// The channel's own texture is attached to a framebuffer of this read's own, rather than the
    /// page it was written on being bound and the channel named as an attachment of that: how many
    /// channels a page holds is whatever the context turned out to allow rather than the four one
    /// is promised, so a channel named by its own number can fall past the last attachment its page
    /// has an image at. A read of an attachment with no image leaves the pixels it was given
    /// untouched and raises an error, which is a black frame to anyone who did not ask for one.
    pub fn read(&self, gl: &glow::Context, at: usize) -> Result<Vec<f32>, String> {
        let texture = self
            .channel(at)
            .ok_or_else(|| format!("the frame has no buffer {at}"))?;
        let count = (self.size.0 * self.size.1 * 4) as usize;
        unsafe {
            let held = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(held));
            gl.framebuffer_texture_2d(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            gl.read_buffer(glow::COLOR_ATTACHMENT0);
            let status = gl.check_framebuffer_status(glow::READ_FRAMEBUFFER);
            let incomplete = (status != glow::FRAMEBUFFER_COMPLETE).then_some(status);
            while gl.get_error() != glow::NO_ERROR {}
            // The G-buffer holds bytes and the frame the composite resolved does not, and a read is
            // only asked for in a pair the format it is reading answers in.
            let values = match at >= TARGETS {
                true => {
                    let mut values = vec![0f32; count];
                    gl.read_pixels(
                        0,
                        0,
                        self.size.0,
                        self.size.1,
                        glow::RGBA,
                        glow::FLOAT,
                        glow::PixelPackData::Slice(Some(bytemuck::cast_slice_mut(&mut values))),
                    );
                    values
                }
                false => {
                    let mut values = vec![0u8; count];
                    gl.read_pixels(
                        0,
                        0,
                        self.size.0,
                        self.size.1,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut values)),
                    );
                    values
                        .into_iter()
                        .map(|value| f32::from(value) / 255.0)
                        .collect()
                }
            };
            let why = gl.get_error();
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            gl.delete_framebuffer(held);
            match incomplete.or((why != glow::NO_ERROR).then_some(why)) {
                Some(why) => Err(format!("buffer {at} would not read back: {why:#x}")),
                None => Ok(values),
            }
        }
    }

    /// Asks the context how much of the G-buffer one pass may write, which is only answerable where
    /// there is a context to ask.
    ///
    /// Asked by every frame rather than by the first one to draw the G-buffer: a material's passes
    /// are translated into pages of this size before the callback that draws them, so a frame that
    /// first asked as it drew would have translated its own passes against the four a context is
    /// promised and written that many targets into a buffer of however many it turned out to allow.
    pub fn limit(&mut self, gl: &glow::Context) {
        if self.attachments == 0 {
            let limit = unsafe { gl.get_parameter_i32(glow::MAX_DRAW_BUFFERS) };
            self.attachments = (limit.max(1) as usize).min(TARGETS);
        }
    }

    /// Every buffer of the graph, sized to what is being drawn into.
    ///
    /// The G-buffer is one framebuffer per page of its targets: a context is promised four draw
    /// buffers and a framebuffer no more color attachments than that, so five targets cannot all
    /// hang off one, and what a page cannot hold is written by a reading of its own.
    pub fn attach(&mut self, gl: &glow::Context, size: (i32, i32)) -> Result<(), String> {
        self.limit(gl);
        if !self.frames.is_empty() && self.size == size {
            return Ok(());
        }
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.color.drain(..).map(Dead::Texture));
        dead.extend(self.depth.take().map(Dead::Texture));
        dead.extend(self.resolved.take().map(Dead::Texture));
        dead.extend(self.frames.drain(..).map(Dead::Frame));
        for (frame, textures) in [
            self.position
                .take()
                .map(|(frame, held)| (frame, vec![held])),
            self.light
                .take()
                .map(|(frame, held)| (frame, held.to_vec())),
            self.lit.take().map(|(frame, held)| (frame, vec![held])),
            self.fur.take().map(|(frame, held)| (frame, vec![held])),
            self.smoothed
                .take()
                .map(|(frame, held)| (frame, vec![held])),
            self.scaled.take().map(|(frame, held)| (frame, vec![held])),
            self.gathered
                .take()
                .map(|(frame, held)| (frame, held.to_vec())),
            self.occluded
                .take()
                .map(|(frame, held)| (frame, vec![held])),
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
            self.position = Some((frame_of(gl, &[position], None)?, position));
            let light = [
                plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?,
                plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?,
            ];
            self.light = Some((frame_of(gl, &light, None)?, light));
            // The composite encodes what it writes but does not bring it under one, so this holds
            // what a byte cannot. It carries the G-buffer's own depth, since a material resolves
            // itself into it as geometry, and a framebuffer with no depth buffer passes every depth
            // test put to it.
            let lit = plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?;
            self.lit = Some((frame_of(gl, &[lit], Some(depth))?, lit));
            self.resolved = Some(plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?);
            // The channel the fur pass answers into is one of the G-buffer's own, reached through a
            // framebuffer of its own so a pass writing one channel does not have to name a page.
            let softened = plane(gl, size, glow::RGBA8, glow::RGBA, glow::UNSIGNED_BYTE)?;
            let held = frame_of(gl, &self.color[FUR_CHANNEL..=FUR_CHANNEL], None)?;
            self.fur = Some((held, softened));

            let smoothed = plane(gl, size, glow::RGBA16F, glow::RGBA, glow::FLOAT)?;
            smooth(gl, smoothed);
            self.smoothed = Some((frame_of(gl, &[smoothed], None)?, smoothed));

            // Whole floats: the pass that fills the first of these answers with the distance in
            // front of the camera, which it caps at a hundred thousand.
            let held = self.fraction();
            let scaled = plane(gl, held, glow::RG32F, glow::RG, glow::FLOAT)?;
            self.scaled = Some((frame_of(gl, &[scaled], None)?, scaled));
            let gathered = [
                plane(gl, held, glow::RGBA32F, glow::RGBA, glow::FLOAT)?,
                plane(gl, held, glow::RGBA32F, glow::RGBA, glow::FLOAT)?,
            ];
            self.gathered = Some((frame_of(gl, &gathered, None)?, gathered));
            let occluded = plane(gl, held, glow::RGBA8, glow::RGBA, glow::UNSIGNED_BYTE)?;
            smooth(gl, occluded);
            self.occluded = Some((frame_of(gl, &[occluded], None)?, occluded));
        }
        Ok(())
    }

    /// What the occlusion chain draws into, which is the frame taken down by the factor the pass
    /// that fills it is named for.
    fn fraction(&self) -> (i32, i32) {
        let held = program::OCCLUSION_SCALE;
        ((self.size.0 / held).max(1), (self.size.1 / held).max(1))
    }

    /// Takes a copy of the resolved frame, which is what the passes drawn over it read. Bound as
    /// the read framebuffer rather than blitted: the draw that follows writes the frame this was
    /// copied from, and a texture being written cannot also be sampled.
    pub fn keep(&self, gl: &glow::Context) -> Result<(), String> {
        let (frame, _) = self.lit.ok_or("no lit frame")?;
        let held = self.resolved.ok_or("no resolved frame")?;
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(frame));
            gl.read_buffer(glow::COLOR_ATTACHMENT0);
            // Onto the first unit rather than whichever happens to be active, which would be a
            // sampler the pass before this one had just been given.
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(held));
            gl.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, 0, 0, self.size.0, self.size.1);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        }
        Ok(())
    }

    /// The resolved frame put through the game's own grading pass.
    ///
    /// That pass saturates what it reads before it reads its table, so the shoulder runs first: it
    /// stands where the game's exposure and tone curve would, and running it here rather than at the
    /// present is what keeps it from being applied twice. Each step reads the copy the one before it
    /// left, since a texture being written cannot also be sampled.
    pub fn post(
        &mut self,
        gl: &glow::Context,
        held: &program::Program,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let (lit, _) = self.lit.ok_or("no lit frame")?;
        let table = held
            .textures
            .iter()
            .find(|texture| texture.name == program::POST_TABLE)
            .and_then(|texture| self.supplied(texture.kind, GRADING.0))
            .ok_or("the grading table has not arrived")?;
        let source = self.resolved.ok_or("no resolved frame")?;
        // The pass that puts a frame up drops the pixels the depth buffer says nothing drew at,
        // which belong to egui. Here every pixel is the frame's, and the depth buffer is attached to
        // what this draws into: sampling it would be reading a buffer it is writing.
        let covered = self.stand_in(gl)?;
        let layout = self.screen(gl)?;
        let shoulder = self.presenter(gl)?;
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            gl.depth_mask(false);
            gl.color_mask(true, true, true, true);
            gl.viewport(0, 0, self.size.0, self.size.1);
        }

        self.keep(gl)?;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(lit));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.use_program(Some(shoulder));
            sampler(gl, shoulder, "u_frame", 0, source);
            sampler(gl, shoulder, "u_depth", 1, covered);
            if let Some(location) = gl.get_uniform_location(shoulder, "u_tone") {
                gl.uniform_1_i32(Some(&location), 1);
            }
            gl.bind_vertex_array(Some(layout));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }

        self.keep(gl)?;
        let program = link(gl, &mut self.resolvers, POST, held)?;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(lit));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.use_program(Some(program));
        }
        self.bind(gl, program, held, scene, &[])?;
        // Named rather than looked up by resource id: what a name here does not reach would be
        // bound the flat stand-in, and a table of one grey renders something plausible and wrong.
        for (unit, texture) in held.textures.iter().enumerate() {
            let bound = match texture.name.as_str() {
                program::POST_INPUT => source,
                program::POST_TABLE => table,
                name => {
                    return Err(format!(
                        "the grading pass reads {name}, which nothing fills"
                    ));
                }
            };
            bind(
                gl,
                program,
                &texture.name,
                unit as u32,
                bound,
                target(texture.kind),
            );
        }
        unsafe {
            gl.bind_vertex_array(Some(layout));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
        }
        self.toned = true;
        Ok(())
    }

    /// How much of the sky reaches each pixel, worked out off the G-buffer before anything lights
    /// it. Three passes at a fraction of the frame: the depth linearized and the normal brought in
    /// front of the camera, a square of four of those packed into the channels of one texel, and the
    /// taps that read them. What it leaves is what every lighting pass and the composite read back.
    pub fn occlude(
        &mut self,
        gl: &glow::Context,
        held: &Occlusion,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let (scaled, _) = self.scaled.ok_or("no scaled depth")?;
        let (gathered, _) = self.gathered.ok_or("no gathered depth")?;
        let (occluded, _) = self.occluded.ok_or("no occlusion")?;
        let size = self.fraction();
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            gl.depth_mask(false);
            // Both later passes discard where the pixel stands past the distance the settings state,
            // and what a discard leaves behind is whatever the buffer last held.
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(occluded));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
            gl.viewport(0, 0, size.0, size.1);
            gl.clear_color(1.0, 1.0, 1.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.pass(gl, 8, &held.scale, (scaled, size), scene, Over::Screen)?;
        self.pass(gl, 9, &held.gather, (gathered, size), scene, Over::Screen)?;
        self.pass(gl, 10, &held.occlude, (occluded, size), scene, Over::Screen)?;
        self.occluding = true;
        Ok(())
    }

    /// Leaves every pass reading the flat stand-in again, which is what a frame nobody asked for
    /// occlusion on is lit against.
    pub fn unocclude(&mut self) {
        self.occluding = false;
    }

    /// The frame with its edges smoothed, which is the last thing the graph does to it.
    ///
    /// Two passes rather than one: the game works each pixel's brightness out in a pass of its own
    /// and leaves it in the alpha the pass after it reads its edges off. Each reads the copy the one
    /// before it left, since a texture being written cannot also be sampled.
    pub fn antialias(
        &mut self,
        gl: &glow::Context,
        held: &Smoothing,
        scene: &program::Scene,
    ) -> Result<(), String> {
        let (lit, _) = self.lit.ok_or("no lit frame")?;
        let (into, smoothed) = self.smoothed.ok_or("no smoothed frame")?;
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            gl.depth_mask(false);
        }
        self.keep(gl)?;
        self.pass(gl, 6, &held.luma, (into, self.size), scene, Over::Screen)?;
        self.pass(gl, 7, &held.fxaa, (lit, self.size), scene, Over::Reading(smoothed))
    }

    /// The framebuffer the composite resolved into, which is what a pass drawn over the frame
    /// writes.
    pub fn frame(&self) -> Option<glow::Framebuffer> {
        self.lit.map(|(frame, _)| frame)
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
            // Said rather than assumed: the pass that puts the frame on screen reads this value
            // back as the sign that nothing drew.
            gl.clear_depth_f32(1.0);
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

    /// The pair that bends a frame toward what a screen holds and puts it up, built the first time
    /// something draws it.
    fn presenter(&mut self, gl: &glow::Context) -> Result<glow::Program, String> {
        if let Some(held) = self.present {
            return Ok(held);
        }
        let held = build_pair(gl, PRESENT_VERTEX, PRESENT_FRAGMENT)?;
        self.present = Some(held);
        Ok(held)
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
        self.blank(gl, glow::TEXTURE_2D)
    }

    /// The same, at whichever target a sampler was declared over.
    fn blank(&mut self, gl: &glow::Context, target: u32) -> Result<glow::Texture, String> {
        if let Some(held) = self.blanks.get(&target) {
            return Ok(*held);
        }
        let held = flat(gl, target, &STAND_IN)?;
        self.blanks.insert(target, held);
        Ok(held)
    }

    /// One of those tables, at the value that leaves the term it drives where it was.
    fn neutral(
        &mut self,
        gl: &glow::Context,
        id: u32,
        value: &[u8; 4],
    ) -> Result<glow::Texture, String> {
        if let Some(held) = self.neutrals.get(&id) {
            return Ok(*held);
        }
        let held = flat(gl, glow::TEXTURE_2D, value)?;
        self.neutrals.insert(id, held);
        Ok(held)
    }

    /// What a lighting pass reads where nothing occluded the pixel. Nothing here computes occlusion,
    /// so every pixel answers the same, and it is not the value a color map would stand in with.
    fn unoccluded(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.unoccluded {
            return Ok(held);
        }
        let held = flat(gl, glow::TEXTURE_2D, &UNOCCLUDED)?;
        self.unoccluded = Some(held);
        Ok(held)
    }

    /// The table `SV_Target.w` indexes, which every pixel shader that shades a surface reads. Empty
    /// until the files it is filled from arrive, which is the branch a plain surface takes.
    pub fn types(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.types {
            return Ok(held);
        }
        let held = dwords(gl, &program::shader_types(&[]))?;
        self.types = Some(held);
        Ok(held)
    }

    /// The same table, as the parameter files that have arrived state it.
    pub fn fill_types(&mut self, gl: &glow::Context, values: &[u32]) -> Result<(), String> {
        let held = dwords(gl, values)?;
        if let Some(stale) = self.types.replace(held) {
            graveyard().lock().unwrap().push(Dead::Texture(stale));
        }
        Ok(())
    }

    /// The cube the composite takes reflections against, before a place has said what its sky looks
    /// like. The alpha is the weight it is blended in at, so nought leaves the ambient it would
    /// otherwise replace.
    pub fn reflection(&mut self, gl: &glow::Context) -> Result<glow::Texture, String> {
        if let Some(held) = self.reflection {
            return Ok(held);
        }
        let held = flat(gl, glow::TEXTURE_CUBE_MAP, &UNREFLECTED)?;
        self.reflection = Some(held);
        Ok(held)
    }

    /// The same cube off the sky a place states, which is what a smooth surface has to reflect where
    /// nothing captures the frame around it. A harmonic row dotted against a direction is what that
    /// sky looks like that way, and a mirror reflection is that same sky read the mirror way.
    ///
    /// Gamma-encoded, since a composite squares the texel and divides it by the alpha it was
    /// gathered at rather than reading it as the light it stands for.
    ///
    /// Rebuilt only where the sky changed: a zone states one per time of day, and a frame otherwise
    /// asks for the same cube it asked for last.
    pub fn reflect(&mut self, gl: &glow::Context, held: &program::Ambient) -> Result<(), String> {
        let sky = (held.sky, held.sky_scale);
        if self.sky == Some(sky) {
            return Ok(());
        }
        let mut pixels = Vec::with_capacity((6 * SKY_FACE * SKY_FACE * 4) as usize);
        for face in 0..6 {
            for y in 0..SKY_FACE {
                for x in 0..SKY_FACE {
                    let over = |at: i32| 2.0 * (at as f32 + 0.5) / SKY_FACE as f32 - 1.0;
                    let toward = facing(face, over(x), over(y)).extend(1.0);
                    pixels.extend(held.sky.iter().map(|row| {
                        let held = row.dot(toward) * sky.1;
                        (held.clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
                    }));
                    pixels.push(255);
                }
            }
        }
        unsafe {
            let texture = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_CUBE_MAP, Some(texture));
            let face = (SKY_FACE * SKY_FACE * 4) as usize;
            for at in 0..6 {
                gl.tex_image_2d(
                    glow::TEXTURE_CUBE_MAP_POSITIVE_X + at as u32,
                    0,
                    glow::RGBA8 as i32,
                    SKY_FACE,
                    SKY_FACE,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&pixels[at * face..(at + 1) * face])),
                );
            }
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_CUBE_MAP, name, value as i32);
            }
            if let Some(stale) = self.reflection.replace(texture) {
                graveyard().lock().unwrap().push(Dead::Texture(stale));
            }
        }
        self.sky = Some(sky);
        Ok(())
    }

    /// Takes one of the game's own textures onto the card, as a plane where it holds one slice and
    /// an array where it holds several: a sampler is declared over one or the other, and a draw only
    /// validates where the texture bound to its unit is of the declaration's own kind.
    ///
    /// An array repeats, since the shaders that read one scale the coordinate up by a tile factor
    /// and expect the tile to come round again. A plane is clamped: it is addressed over its whole
    /// width, and wrapping would blend its last texel against its first. What is read between the
    /// texels is the caller's to say, since nothing about the texture tells whether it is an image
    /// or a table.
    pub fn layered(&mut self, gl: &glow::Context, id: u32, held: &Layered) -> Result<(), String> {
        let (target, wrap) = match (held.volumetric, held.layers > 1) {
            (true, _) => (glow::TEXTURE_3D, glow::CLAMP_TO_EDGE),
            (false, true) => (glow::TEXTURE_2D_ARRAY, glow::REPEAT),
            (false, false) => (glow::TEXTURE_2D, glow::CLAMP_TO_EDGE),
        };
        unsafe {
            let texture = gl.create_texture()?;
            gl.bind_texture(target, Some(texture));
            match target {
                glow::TEXTURE_2D => gl.tex_image_2d(
                    target,
                    0,
                    glow::RGBA8 as i32,
                    held.size.0,
                    held.size.1,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&held.pixels)),
                ),
                _ => gl.tex_image_3d(
                    target,
                    0,
                    glow::RGBA8 as i32,
                    held.size.0,
                    held.size.1,
                    held.layers,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&held.pixels)),
                ),
            }
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, held.filter),
                (glow::TEXTURE_MAG_FILTER, held.filter),
                (glow::TEXTURE_WRAP_S, wrap),
                (glow::TEXTURE_WRAP_T, wrap),
            ] {
                gl.tex_parameter_i32(target, name, value as i32);
            }
            if target == glow::TEXTURE_3D {
                gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_R, wrap as i32);
            }
            if let Some((_, stale)) = self.arrays.insert(id, (target, texture)) {
                graveyard().lock().unwrap().push(Dead::Texture(stale));
            }
        }
        Ok(())
    }

    /// The file a resource id names, where one has arrived and its own target is the one a sampler
    /// of this kind reads through. A file states how many slices it holds and a package states what
    /// it is sampled as, so the two can disagree; a texture bound at the other target is not of the
    /// declaration's kind, and answers a constant rather than being rejected.
    fn supplied(&self, kind: program::Kind, id: u32) -> Option<glow::Texture> {
        self.arrays
            .get(&id)
            .filter(|(at, _)| *at == target(kind))
            .map(|(_, held)| *held)
    }

    /// What stands in for a sampler of this kind that nothing bound a texture to: the file the
    /// resource id names once that has arrived, and the flat texture of its own target until then.
    pub fn absent(
        &mut self,
        gl: &glow::Context,
        kind: program::Kind,
        id: u32,
    ) -> Result<glow::Texture, String> {
        match kind {
            program::Kind::Cube => self.reflection(gl),
            held => match self.supplied(held, id) {
                Some(texture) => Ok(texture),
                None => self.blank(gl, target(held)),
            },
        }
    }

    /// Every stand-in a draw may reach for, made before any of them is bound.
    ///
    /// Making a texture binds it to whichever unit happens to be active, so one made partway through
    /// a binding loop takes over the unit the sampler before it was given, and that sampler then
    /// reads a texture of the wrong format.
    pub fn stand_ins(&mut self, gl: &glow::Context) -> Result<(), String> {
        self.unoccluded(gl)?;
        self.types(gl)?;
        self.reflection(gl)?;
        for (id, value) in &NEUTRAL {
            self.neutral(gl, *id, value)?;
        }
        for kind in [
            program::Kind::Plane,
            program::Kind::Array,
            program::Kind::Volume,
        ] {
            self.blank(gl, target(kind))?;
        }
        Ok(())
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
        if let Some(held) = self.supplied(program::Kind::Plane, id) {
            return Ok(held);
        }
        if let Some(held) = self.neutrals.get(&id) {
            return Ok(*held);
        }
        Ok(match id {
            DEPTH | DEPTH_PLANE => self.depth.ok_or("no depth buffer")?,
            NORMAL_PLANE => self
                .color
                .get(NORMAL_CHANNEL)
                .copied()
                .ok_or("the G-buffer has no normal channel")?,
            VIEW_POSITION => self.position.ok_or("no view position")?.1,
            LIGHT_DIFFUSE => self.light.ok_or("no light buffer")?.1[0],
            LIGHT_SPECULAR => self.light.ok_or("no light buffer")?.1[1],
            FINAL_COLOR | INPUT => self.resolved.ok_or("no resolved frame")?,
            DEPTH_NORMAL_Z => self.scaled.ok_or("no scaled depth")?.1,
            GATHER_DEPTH => self.gathered.ok_or("no gathered depth")?.1[0],
            GATHER_NORMAL_Z => self.gathered.ok_or("no gathered depth")?.1[1],
            OCCLUSION if self.occluding => self.occluded.ok_or("no occlusion")?.1,
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
    /// it left behind.
    fn pass(
        &mut self,
        gl: &glow::Context,
        at: usize,
        held: &program::Program,
        into: (glow::Framebuffer, (i32, i32)),
        scene: &program::Scene,
        over: Over,
    ) -> Result<(), String> {
        let (into, size) = into;
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
        let layout = match over {
            Over::Volume => {
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
            _ => self.screen(gl)?,
        };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(into));
            let written: Vec<u32> = (0..held.targets.len().max(1))
                .map(|slot| glow::COLOR_ATTACHMENT0 + slot as u32)
                .collect();
            gl.draw_buffers(&written);
            gl.viewport(0, 0, size.0, size.1);
            gl.use_program(Some(program));
            gl.color_mask(true, true, true, true);
        }
        self.bind(gl, program, held, scene, &[])?;
        self.stand_ins(gl)?;
        let mut unit = 0;
        for texture in &held.textures {
            let bound = match texture.kind {
                program::Kind::Plane => match over {
                    Over::Softening(held) if texture.id == GBUFFER[3] => held,
                    Over::Reading(held) if texture.id == INPUT => held,
                    _ => self.engine(gl, texture.id)?,
                },
                kind => self.absent(gl, kind, texture.id)?,
            };
            bind(
                gl,
                program,
                &texture.name,
                unit,
                bound,
                target(texture.kind),
            );
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
                gl.uniform_2_f32(Some(&location), size.0 as f32, size.1 as f32);
            }
            // What a pass reading a square of its own source steps between the corners of it.
            if let Some(location) = gl.get_uniform_location(program, "u_texel") {
                gl.uniform_2_f32(Some(&location), 1.0 / size.0 as f32, 1.0 / size.1 as f32);
            }
            gl.bind_vertex_array(Some(layout));
            match over {
                Over::Volume => gl.draw_elements(
                    glow::TRIANGLES,
                    VOLUME_FACES.len() as i32,
                    glow::UNSIGNED_SHORT,
                    0,
                ),
                _ => gl.draw_arrays(glow::TRIANGLES, 0, 3),
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
        self.toned = false;
        self.reflect(gl, &scene.ambient)?;
        unsafe {
            // A screen-wide pass covers every pixel and reads the depth rather than testing against
            // it, so nothing here is depth tested and nothing writes depth.
            gl.disable(glow::DEPTH_TEST);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
        }
        self.pass(gl, 0, &lighting.position, (position, self.size), scene, Over::Screen)?;
        // A strand is softened where it stands rather than where it is lit, so this runs before any
        // light reads the channel. It walks its neighbours to answer for one pixel, which is why it
        // reads a copy of the channel and writes the channel itself, and it discards where the
        // surface states no fur, leaving those pixels as the G-buffer left them.
        if let Some(fur) = &lighting.fur
            && let Some((into, softened)) = self.fur
        {
            unsafe {
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(into));
                gl.read_buffer(glow::COLOR_ATTACHMENT0);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(softened));
                gl.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, 0, 0, self.size.0, self.size.1);
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            }
            let held = fur.clone();
            self.pass(gl, 5, &held, (into, self.size), scene, Over::Softening(softened))?;
        }

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(light));
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0, glow::COLOR_ATTACHMENT1]);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
        }
        self.pass(gl, 1, &lighting.directional, (light, self.size), scene, Over::Screen)?;
        // One face of the volume, not both. The pass adds what it computes to the buffer, so a box
        // shaded front and back would light every pixel it covers twice over. The far face is the
        // one kept, since it still covers the frame when the camera stands inside the light.
        unsafe {
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::FRONT);
            gl.front_face(glow::CCW);
        }
        // Every lamp of a kind is the same pass over a volume of its own, so each kind's program is
        // linked once, at a slot of its own, and only the buffer it reads is written again. A spot
        // whose package has not arrived draws through the point one, which reads neither its
        // direction nor its cone: a lit box rather than nothing.
        for lamp in lamps {
            let held = program::Scene {
                lamp: *lamp,
                ..*scene
            };
            let (slot, program) = match (lamp.kind, &lighting.spot) {
                (program::LampKind::Spot, Some(spot)) => (3, spot),
                _ => (2, &lighting.point),
            };
            self.pass(gl, slot, program, (light, self.size), &held, Over::Volume)?;
        }
        unsafe {
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
        };
        self.pass(gl, 4, &lighting.composite, (lit, self.size), scene, Over::Screen)
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.color.drain(..).map(Dead::Texture));
        dead.extend(
            [
                self.types.take(),
                self.unoccluded.take(),
                self.reflection.take(),
                self.resolved.take(),
                self.depth.take(),
            ]
            .into_iter()
            .flatten()
            .chain(std::mem::take(&mut self.blanks).into_values())
            .chain(std::mem::take(&mut self.neutrals).into_values())
            .chain(
                std::mem::take(&mut self.arrays)
                    .into_values()
                    .map(|(_, held)| held),
            )
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
            self.fur.take().map(|(frame, held)| (frame, vec![held])),
            self.smoothed
                .take()
                .map(|(frame, held)| (frame, vec![held])),
            self.scaled.take().map(|(frame, held)| (frame, vec![held])),
            self.gathered
                .take()
                .map(|(frame, held)| (frame, held.to_vec())),
            self.occluded
                .take()
                .map(|(frame, held)| (frame, vec![held])),
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

/// The target a texture a sampler of this kind reads has to be bound at.
pub fn target(kind: program::Kind) -> u32 {
    match kind {
        program::Kind::Plane => glow::TEXTURE_2D,
        program::Kind::Array => glow::TEXTURE_2D_ARRAY,
        program::Kind::Volume => glow::TEXTURE_3D,
        program::Kind::Cube => glow::TEXTURE_CUBE_MAP,
    }
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

/// The exception: the one buffer a pass reads between texels rather than at their centres.
fn smooth(gl: &glow::Context, texture: glow::Texture) {
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for name in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::LINEAR as i32);
        }
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
fn frame_of(
    gl: &glow::Context,
    color: &[glow::Texture],
    depth: Option<glow::Texture>,
) -> Result<glow::Framebuffer, String> {
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
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::DEPTH_ATTACHMENT,
            glow::TEXTURE_2D,
            depth,
            0,
        );
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
    bind(gl, program, name, unit, texture, glow::TEXTURE_2D);
}

/// The same over whichever target the shader declared the sampler against. A draw only validates if
/// the texture bound to a unit is of the sampler's own type, so the target follows the declaration.
pub fn bind(
    gl: &glow::Context,
    program: glow::Program,
    name: &str,
    unit: u32,
    texture: glow::Texture,
    target: u32,
) {
    unsafe {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(target, Some(texture));
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

/// A one-texel texture of the given target, answering with the same value everywhere. A cube states
/// each of its faces and the targets that take a depth state one slice.
fn flat(gl: &glow::Context, target: u32, value: &[u8; 4]) -> Result<glow::Texture, String> {
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(target, Some(texture));
        match target {
            glow::TEXTURE_CUBE_MAP => {
                for face in 0..6 {
                    gl.tex_image_2d(
                        glow::TEXTURE_CUBE_MAP_POSITIVE_X + face,
                        0,
                        glow::RGBA as i32,
                        1,
                        1,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(value)),
                    );
                }
            }
            glow::TEXTURE_2D_ARRAY | glow::TEXTURE_3D => gl.tex_image_3d(
                target,
                0,
                glow::RGBA as i32,
                1,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(value)),
            ),
            _ => gl.tex_image_2d(
                target,
                0,
                glow::RGBA as i32,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(value)),
            ),
        }
        for (name, held) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(target, name, held as i32);
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
