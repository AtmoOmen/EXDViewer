//! A model drawn with the shaders the game would draw it with.
//!
//! The material names a package and a set of keys; the package's node table turns those into the
//! vertex and pixel shader of one pass, and both are translated to GLSL ES 3.00. What the shaders
//! then read is the material's own textures and color table, the package's own parameter buffer, and
//! a camera reconstructed field by field off each buffer's reflection.
//!
//! The G-buffer is five targets and a context is only promised four draw buffers, so the pixel
//! shader is emitted with one page of its outputs at a time: a shader declaring a location the
//! context has no draw buffer for would not link, which makes the split a translation-time choice.

use std::collections::{BTreeSet, HashMap};

use glam::{Mat4, Vec3, Vec4};
use ironworks::file::shpk::{self, ShaderPackage, Stage};
use ironworks::file::{mtrl, shcd, spm};

use super::material::{Family, Material};

/// The passes a model is drawn with.
const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const PASS_G_SEMITRANSPARENCY: u32 = 0x6006_067f;
const PASS_Z_OPAQUE: u32 = 0xe412_a2d4;
const PASS_LIGHTING_OPAQUE: u32 = 0xfbde_0a8f;
const PASS_COMPOSITE_OPAQUE: u32 = 0x955c_0b73;
const PASS_COMPOSITE_SEMITRANSPARENCY: u32 = 0xc885_bbd3;
/// What a surface that lights itself answers into the frame with. Water resolves through this rather
/// than through the composite pass a glass package takes.
const PASS_LIGHTING_SEMITRANSPARENCY: u32 = 0x1f19_7698;
const PASS_WATER: u32 = 0x8ef4_0d56;

/// The pass with no name of its own, which two packages use for the one thing each of them does:
/// furblur marches along a strand at it, and every cloud node holds it and nothing else.
const PASS_7: u32 = 0x5bc1_ad3f;

/// The render pass a node is selected under. Holding everything else fixed, a drawing package
/// answers `SUB_VIEW_SHADOW_0` with its depth pass alone, which is what a shadow map is.
pub const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;
pub const SUB_VIEW_SHADOW_0: u32 = 0x99b2_2d1c;
pub const SUB_VIEW_CUBE_0: u32 = 0x6624_4231;
pub const SUB_VIEW_ROOF: u32 = 0xae5e_6a42;
pub const SUB_VIEW_MAIN_SELECT: u32 = 0x0c01_20ca;

/// The packages the frame is lit and resolved with, in the order they run, and the pass each is run
/// under. What each reads is what the one before it wrote.
pub const VIEW_POSITION: &str = "shader/sm5/shpk/createviewposition.shpk";
pub const DIRECTIONAL: &str = "shader/sm5/shpk/directionallighting.shpk";
pub const POINT: &str = "shader/sm5/shpk/pointlighting.shpk";
pub const SPOT: &str = "shader/sm5/shpk/spotlighting.shpk";
pub const LINE: &str = "shader/sm5/shpk/linelighting.shpk";
pub const PLANE: &str = "shader/sm5/shpk/planelighting.shpk";
pub const SHADOW: &str = "shader/sm5/shpk/directionalshadow.shpk";
pub const COMPOSITE: &str = "shader/sm5/shpk/bg_composite.shpk";
/// Softens the surface a strand grows out of, between the G-buffer and the light it is read under.
pub const FUR: &str = "shader/sm5/shpk/furblur.shpk";
pub const SCATTER: &str = "shader/sm5/shpk/subsurfaceblur.shpk";

/// The members of the game's post chain the viewer runs. The first reads a table a file holds. The
/// other two smooth the frame's edges, in the order they run: one writes each pixel's brightness
/// into the alpha the next reads its edges off.
pub const TONE_ADJUST: &str = "shader/sm5/posteffect/ToneAdjust.shcd";
pub const FXAA_LUMA: &str = "shader/sm5/posteffect/FXAALuma.shcd";
pub const FXAA: &str = "shader/sm5/posteffect/FXAA.shcd";

/// How bright the frame turned out and what that does to it. The first three halve the frame down
/// to one texel, accumulating the reciprocal of each tap's luminance, so what lands is a harmonic
/// mean; the fourth carries that toward the last frame's rather than jumping to it; the fifth builds
/// the curve as a 1024-wide strip; the last reads the frame through it.
pub const MEASURE_INITIAL: &str = "shader/sm5/posteffect/MeasureLumInitial.shcd";
pub const MEASURE_ITERATIVE: &str = "shader/sm5/posteffect/MeasureLumIterative.shcd";
pub const MEASURE_FINAL: &str = "shader/sm5/posteffect/MeasureLumFinal.shcd";
pub const ADAPT_LUM: &str = "shader/sm5/posteffect/AdaptLum.shcd";
pub const TONE_MAP_LUT: &str = "shader/sm5/posteffect/ToneMapLut.shcd";
pub const TONE_MAPPING: &str = "shader/sm5/posteffect/ToneMapping.shcd";

/// The six of them, for asking after at once.
pub const MEASURE: [&str; 6] = [
    MEASURE_INITIAL,
    MEASURE_ITERATIVE,
    MEASURE_FINAL,
    ADAPT_LUM,
    TONE_MAP_LUT,
    TONE_MAPPING,
];

/// Texels of the curve the tone pass reads, which the buffer states as the half-texel bounds
/// `(0.5/1024, 1 - 0.5/1024)`.
pub const CURVE: i32 = 1024;

/// The sky, drawn over whatever the frame did not cover.
pub const SKY: &str = "shader/sm5/posteffect/Sky.shcd";

/// The volume one is read out of. A sky is a strip a few texels wide by a few dozen tall, stacked
/// once per hour of the day, and the id picks which of them a place stands under.
pub fn sky_texture(id: u16) -> String {
    format!("bgcommon/nature/sky/texture/sky_{id:03}.tex")
}

/// The sun's own glow, drawn over the sky: a screen-wide pass that measures every pixel against
/// where the sun stands and answers a core, six rays and a wide halo.
pub const SUN: &str = "shader/sm5/posteffect/Sun.shcd";

/// Every lane of `cSunParam` past the two the frame decides, read out of the one draw a real frame
/// makes of this pass. The falloffs are in half-frame-heights rather than pixels, so they carry
/// across resolutions: the pass measures its radius after scaling x by the aspect.
///
/// The frame that holds them had the sun far off screen, so the rays and the core are live buffer
/// contents that nothing has been seen to rasterize.
const SUN_RAYS: [f32; 4] = [3.0, 0.965_246_44, 4.983_494_3, 0.499_174_03];
const SUN_FALLOFF: [f32; 4] = [-66.537_23, -92.146_774, -127.613_18, -575.917_3];
const SUN_CORE: [f32; 4] = [
    std::f32::consts::SQRT_2,
    1.0,
    std::f32::consts::FRAC_1_SQRT_2,
    1.0,
];
const SUN_HALO: [f32; 4] = [1.347_855_2, 1.350_965_7, 1.350_469_2, 0.1];

/// The clouds, which the engine draws over two meshes it builds itself: a band around the horizon
/// and a sheet overhead. One package holds both, under a technique apiece.
pub const CLOUD: &str = "shader/sm5/shpk/cloud.shpk";
pub const CLOUD_BAND: u32 = 0xa2f7_6b97;
pub const CLOUD_SHEET: u32 = 0xd9d5_8038;

/// The textures each draws, which the environment's cloud set names by id. A sheet of nought is a
/// weather that draws none: no such file exists.
pub fn cloud_texture(id: u16) -> String {
    format!("bgcommon/nature/cloud/texture/cloud_{id:03}.tex")
}

pub fn cloudside_texture(id: u16) -> String {
    format!("bgcommon/nature/cloud/texture/cloudside_{id:03}.tex")
}

/// The fog, which drags a distant pixel toward the color the weather states and a further one toward
/// the sky itself, and hazes a near one by how much air stands between it and the camera.
///
/// The install ships this under no name: the shader category records 319 files against 318 known
/// names, and the one left over is this. `Fog.shcd` is an older shader that runs in no frame the
/// game draws. Named the way the asset browser shows an unnamed file, and read the same way.
pub const FOG: &str = "shader/sm5/posteffect/e8bf3721";
pub const FOG_DIRECTORY: &str = "shader/sm5/posteffect";

/// Texels of the table it reads the curve out of, which is what its own scale and bias address.
pub const FOG_TABLE: i32 = 256;

/// What the pass reads: the frame's depth, the sky on a plane of its own, and that table.
pub const FOG_DEPTH: &str = "sDepth";
pub const FOG_SKY: &str = "sSky";
pub const FOG_LUT: &str = "sLut";

/// The two passes that stand between the G-buffer and the occlusion read off it: one linearizes the
/// depth and brings the normal into view space, the other packs a square of four of those into the
/// channels of one texel, which is the shape the occlusion pass addresses.
pub const DOWN_SCALE: &str = "shader/sm5/posteffect/DownScaleDepthNormalZ.shcd";
pub const GATHER: &str = "shader/sm5/posteffect/GatherDepthNormalZ.shcd";

/// What each of those runs at, against the frame. The pass that fills the first is named for the
/// scaling and gathers a square of the depth buffer per texel, so this is the factor that makes that
/// gather the two-by-two it is written as.
pub const OCCLUSION_SCALE: i32 = 2;

/// What the occlusion pass reads and over how many taps, at each quality the game ships. Its file
/// is `SSAO` and the place in this list: the four depth-only readings first, then the four that read
/// the normal too, each set running the same taps as the other.
pub const OCCLUDERS: [&str; 8] = [
    "2 taps, depth",
    "6 taps, depth",
    "12 taps, depth",
    "20 taps, depth",
    "2 taps, depth and normal",
    "6 taps, depth and normal",
    "12 taps, depth and normal",
    "20 taps, depth and normal",
];

/// The buffers the exposure chain reads, and the frame, the measure and the table the passes read
/// them through. `cToneMapParam` is shared with the grading pass, which reads the same two lanes as
/// something else entirely: `.z` as how much of its table reaches the frame, `.w` as an exponent.
const TONE_MAP_PARAM: &str = "cToneMapParam";
const ADAPT_LUM_PARAM: &str = "cAdaptLumParam";
const SKY_PARAM: &str = "cSkyParam";
const SUN_PARAM: &str = "cSunParam";
const FOG_PARAM: &str = "cFogParam";
const HEIGHT_FOG_PARAM: &str = "cExpHeightFogParam";
const DIRECTIONAL_SHADOW_PARAM: &str = "g_DirectionalShadowParameter";
const SHADOW_BIAS_PARAM: &str = "g_ShadowBiasParameter";

/// How many taps the shadow resolve reads. One is a single comparison and shows every texel of the
/// map as a step; nine is what softens the edge.
pub const SHADOW_SOFT: u32 = 0xa89d_89f0;
pub const SHADOW_SOFT_3X3: u32 = 0x9915_3ff0;

/// How wide the sun's own map is drawn, which is what a texel of it measures.
pub const SHADOW_MAP: i32 = 2048;
const CLIP_TO_WORLD: &str = "cC2W";

/// What the cloud draws read themselves against. Both are named the way any package names the two
/// buffers its own stages take, so nothing but a cloud pass fills them this way.
const VS_PARAM: &str = "g_VSParam";
const PS_PARAM: &str = "g_PSParam";

/// How far the sheet's texture tiles across the forty thousand units it spans, which puts one period
/// of it every four thousand.
const SHEET_TILING: f32 = 10.0;
const SHEET_SPAN: f32 = 20000.0;
const SHEET_HEIGHT: f32 = 2000.0;
const SHEET_RISE: f32 = 1000.0;

/// The radius the band stands at around the camera.
const BAND_RADIUS: f32 = 2000.0;

/// How far the view direction is carried toward straight down before a cloud is lit against it, and
/// the alpha the sheet fades toward overhead. One number does both, and it is a single sample: only
/// the sheet's own shaders read it, and only one frame measured holds a sheet.
const CLOUD_FLOOR: f32 = 0.5;
const PROJECTION_INVERSE: &str = "cProjInv";
const VIEW_INVERSE: &str = "cViewInv";
const COMMON_TEX_PARAM: &str = "cCommonTexParam";
pub const POST_INPUT: &str = "sInput";
pub const POST_TABLE: &str = "sLUT";
pub const POST_MEASURE: &str = "sToneMap";
pub const POST_ADAPTED: &str = "sAdaptedLum";

/// What the grading pass takes of that buffer. Neither lane is stated anywhere: the environment's
/// colour filter set carries a grading beside the tone mapping one, but nothing pairs its fields
/// with these. So the exponent is left where it changes nothing, and the table is left out: three of
/// them ship and nothing states which one binds, so reading one at full strength grades a frame it
/// was never authored over and takes the color out of it. Only that pass reads the buffer this way,
/// which is why it is the only one built with it.
const TONE_MAP: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The buffers the smoothing pass reads. The first is the rectangle of its target the frame was
/// rendered into, which every pass of the chain clamps its reads to; the frame fills the whole of
/// one here, so the corner it names is the far one.
const VIEWPORT_PARAM: &str = "cDynamicViewportResolutionParam";
const FXAA_PARAM: &str = "cFxaaParam";

/// The buffers the occlusion chain reads. The first is what turns a depth buffer reading into the
/// distance in front of the camera it stands for, and the second the rotation that brings a normal
/// out of the world and into the camera's own space. The third is a bare `float4[3]` the reflection
/// gives no member names and no defaults at all.
const VIEW_DEPTH_FACTOR: &str = "cViewDepthFactor";
const VIEW_ROTATION: &str = "cView";
const HDAO_PARAM: &str = "cHDAOParam";

/// The vertex shader the pass is drawn with. The game pairs these with a `VSSampling`, which reads a
/// quad of positions and coordinates against a scale and a bias no file states; the screen triangle
/// carries its own, and a frame a pass of this graph wrote is already the way round a sampler here
/// reads it.
pub const POST_VERTEX: &str = "\
#version 300 es

layout(location = 0) in vec4 a_position;

out vec2 TEXCOORD;

void main() {
\tTEXCOORD = a_position.xy * 0.5 + 0.5;
\tgl_Position = a_position;
}
";

/// The sky's own, which hands the fragment where it stands in clip space rather than a texture
/// coordinate: the pass unprojects that to find which way the pixel looks. Held at the far plane, so
/// a depth test keeps it behind everything already drawn rather than over it.
pub const SKY_VERTEX: &str = "\
#version 300 es

layout(location = 0) in vec4 a_position;

out vec2 TEXCOORD;

void main() {
\tTEXCOORD = a_position.xy;
\tgl_Position = vec4(a_position.xy, 1.0, 1.0);
}
";

/// The same for the gathering pass, which reads four texels of one square rather than one. The
/// pixel's own texel goes last, since that is the lane the occlusion pass takes its center from, and
/// the other three run round the square so that a lane and the one two along from it stand either
/// side of its middle. That pairing is what the occlusion pass mirrors its taps by.
pub const GATHER_VERTEX: &str = "\
#version 300 es

layout(location = 0) in vec4 a_position;

uniform vec2 u_texel;

out vec4 TEXCOORD;
out vec4 TEXCOORD1;

void main() {
\tvec2 uv = a_position.xy * 0.5 + 0.5;
\tTEXCOORD = vec4(uv + vec2(0.0, u_texel.y), uv + u_texel);
\tTEXCOORD1 = vec4(uv + vec2(u_texel.x, 0.0), uv);
\tgl_Position = a_position;
}
";

/// The same for the passes that halve the frame, which read the four source texels one destination
/// texel covers. The game pairs these with a `VSSampling4`, whose offsets come from a buffer no file
/// states; a halving names its own, since the source texel is half the destination's and the four
/// taps stand at its corners.
pub const SAMPLING_VERTEX: &str = "\
#version 300 es

layout(location = 0) in vec4 a_position;

uniform vec2 u_texel;

out vec4 TEXCOORD;
out vec4 TEXCOORD1;

void main() {
\tvec2 uv = a_position.xy * 0.5 + 0.5;
\tvec2 step = u_texel * 0.25;
\tTEXCOORD = vec4(uv - step, uv + vec2(step.x, -step.y));
\tTEXCOORD1 = vec4(uv + vec2(-step.x, step.y), uv + step);
\tgl_Position = a_position;
}
";

/// `GetDirectionalLight`, and the value that draws a light rather than nothing. The package defaults
/// it to `_Disable`, whose shader writes no light at all.
const GET_DIRECTIONAL_LIGHT: u32 = 0x8115_916d;
const GET_DIRECTIONAL_LIGHT_ENABLE: u32 = 0x51ed_d496;
/// What the shadowed frame will want instead. Left unused until a mask exists to read: drawn with a
/// white stand-in it comes out measurably darker than the enabled variant, and why is not yet known.
const GET_DIRECTIONAL_LIGHT_SHADOW: u32 = 0xd73b_9e89;

/// `ApplyDetailMap`, and the value that lays the tiled detail arrays over a surface. A background
/// package defaults it to `_Disable`, which draws a wall as its own textures and nothing finer.
const APPLY_DETAIL_MAP: u32 = 0x6313_fd87;
const APPLY_DETAIL_MAP_ENABLE: u32 = 0x7a3d_9efd;

/// `SpecularLighting`, and the value that works a specular out rather than moving nought into the
/// target the composite reads it back from. A placed light's package defaults it to `_Disable`.
const SPECULAR_LIGHTING: u32 = 0x0d81_2fa4;
const SPECULAR_LIGHTING_ENABLE: u32 = 0xaba1_f498;

/// Entries the array holds, which is what its own extent divides into twelve registers apiece.
const ENTRIES: usize = 64;

/// Records of `g_ShaderTypeParameter`, which `SV_Target.w` indexes as `(32 + type) / 255`.
const SHADER_TYPES: usize = 256;

/// Dwords of one `g_ShaderTypeParameter` record, and the one the fur pass reads.
const SHADER_TYPE: usize = 32;
const FUR_LENGTH: usize = 12;

/// Where the character family's own profiles start, and what a material with no colour table names
/// its profile with.
const CHARA_TYPES: usize = 32;
const SHADER_ID: u32 = 0x59bd_a0b1;

/// Dwords in a row of the texture a structured buffer is read through, which the backend fixes so
/// that a shader and whatever fills the texture agree without either having to say so.
pub const ROW: usize = hlsl::glsl::ROW as usize;

/// Dwords of one joint's transform: four columns of three floats, densely packed.
const JOINT: usize = 12;

/// The buffer a drawing package reads one record of per object drawn.
const INSTANCING: &str = "g_InstancingData";

/// The buffer holding what the engine decides per object rather than per material.
const INSTANCE: &str = "g_InstanceParameter";

/// Its fields, as every package that reads one by name declares them. `iris.shpk` picks its record
/// out by which eye a vertex belongs to, and a reflection describes a buffer indexed that way as one
/// bare array, so the names have to come from somewhere for a fill to reach it at all.
const INSTANCE_FIELDS: [(&str, u32); 9] = [
    ("m_MulColor", 16),
    ("m_EnvParameter", 16),
    ("m_CameraLight", 32),
    ("m_Wetness", 16),
    ("m_Wind", 16),
    ("m_PrevWind", 16),
    ("m_IrisParam", 32),
    ("m_Param", 16),
    ("m_HeadUpVector", 16),
];

fn instance_fields() -> Vec<hlsl::layout::Member> {
    INSTANCE_FIELDS
        .iter()
        .scan(0, |offset, (name, size)| {
            let at = *offset;
            *offset += size;
            Some(hlsl::layout::Member {
                name: (*name).to_owned(),
                offset: at,
                size: *size,
                kind: "float4".to_owned(),
            })
        })
        .collect()
}

/// Which pass of the node to take. `Lighting` and `Lamp` take the same one: the sun and a placed
/// light are separate packages reading one buffer differently.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Depth,
    Buffer,
    Blended,
    Lighting,
    Lamp,
    Fur,
    CloudBand,
    CloudSheet,
    Composite,
    CompositeBlended,
    /// What a semitransparent surface that lights itself resolves through.
    BlendedLighting,
    /// What water shades itself with, reading the lit frame back rather than filling the G-buffer.
    Water,
}

impl Pass {
    fn id(self) -> u32 {
        match self {
            Self::Depth => PASS_Z_OPAQUE,
            Self::Buffer => PASS_G_OPAQUE,
            Self::Blended => PASS_G_SEMITRANSPARENCY,
            Self::Lighting | Self::Lamp => PASS_LIGHTING_OPAQUE,
            Self::Fur | Self::CloudBand | Self::CloudSheet => PASS_7,
            Self::Composite => PASS_COMPOSITE_OPAQUE,
            Self::CompositeBlended => PASS_COMPOSITE_SEMITRANSPARENCY,
            Self::BlendedLighting => PASS_LIGHTING_SEMITRANSPARENCY,
            Self::Water => PASS_WATER,
        }
    }
}

/// Where in a vertex an attribute reads from. The mesh supplies every semantic a drawing package
/// asks for, and each shader binds the ones its own signature names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Position,
    Normal,
    Tangent,
    Bitangent,
    Uv,
    Uv1,
    Color,
    Color1,
    Weights,
    Bones,
}

/// What a signature declares an attribute's components as. A draw only validates where the pointer
/// reads with a type of the same class, signedness and all.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Components {
    Float,
    Signed,
    Unsigned,
}

/// One vertex attribute, as the shader's own input signature asks for it.
pub struct Attribute {
    pub location: u32,
    pub field: Field,
    pub components: Components,
}

/// What a sampler is declared over. A draw only validates where the texture bound to the unit is of
/// the declaration's own kind, so this is what decides the target it is bound at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plane,
    Array,
    Volume,
    Cube,
}

/// A texture the shader samples, named as GLSL has it and identified as the material names it.
pub struct Texture {
    pub name: String,
    /// The package's own resource id, which is the crc a material's samplers use.
    pub id: u32,
    pub kind: Kind,
}

/// A constant buffer the shader binds, and the fields the reflection describes it with.
pub struct Buffer {
    pub name: String,
    members: Vec<hlsl::layout::Member>,
    registers: u32,
    /// What the files decide a buffer holds, worked out once.
    fixed: Option<Vec<u8>>,
}

/// A structured buffer, which GLSL has no such thing as and reads through a texture of dwords.
pub struct Structured {
    pub name: String,
    pub stride: usize,
}

/// One object of a batch, as `g_InstancingData` holds one.
#[derive(Clone, Copy)]
pub struct Instance {
    /// Where the object stands, in world space.
    pub transform: Mat4,
    /// How much sky reaches it, which a zone states per instance in its `.svb`.
    pub sky_visibility: f32,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            transform: Mat4::IDENTITY,
            sky_visibility: 1.0,
        }
    }
}

/// The shape a placed light throws, which is a package of its own reading the one `g_LightParam`
/// differently. A kind whose package a zone has not fetched draws as a point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LampKind {
    Point,
    Spot,
    /// A light with length rather than a point, and one with area: the file calls them line and
    /// flat, and each has a package of its own that reads the same buffer differently.
    Line,
    Plane,
}

/// One placed light, as `g_LightParam` reads it. The box is the one a zone's `.lcb` clips the light
/// against: stated in the light's own space, in the same units the placement stands in, which is
/// what makes its extent the distance the light carries.
#[derive(Clone, Copy)]
pub struct Lamp {
    /// Takes the light's own space into the world, without scaling it.
    pub placement: Mat4,
    pub min: Vec3,
    pub max: Vec3,
    /// How far the light stays at full strength, which its own record states and the box it is
    /// clipped against does not. The shading saturates an inverse-distance term against it, so a
    /// light given its whole clip volume here is at full strength everywhere inside it and falls off
    /// only over the last tenth of the way out.
    pub range: f32,
    pub color: Vec3,
    pub kind: LampKind,
    /// Which way the light throws, in world space. Its own space points it along positive z: that
    /// is the axis a spot's vertex shader keeps the half of its box on.
    pub direction: Vec3,
    /// The cosine a spot's cone is cut at, which its own shader compares the direction to a pixel
    /// against. Nothing but a spot reads it.
    pub cone: f32,
}

impl Default for Lamp {
    fn default() -> Self {
        Self {
            placement: Mat4::IDENTITY,
            min: Vec3::splat(-1.0),
            max: Vec3::ONE,
            range: 1.0,
            color: Vec3::ONE,
            kind: LampKind::Point,
            direction: Vec3::Z,
            cone: -1.0,
        }
    }
}

impl Lamp {
    /// How far the light carries, which is what its own falloff is scaled by.
    fn reach(&self) -> f32 {
        self.min.abs().max(self.max.abs()).max_element().max(0.001)
    }

    /// The box in units of that reach. The vertex shader clamps a unit cube against it, so a box
    /// stated any larger would leave the corners where they were and the light would draw the cube
    /// rather than the box.
    fn clip(&self) -> (Vec3, Vec3) {
        let reach = self.reach();
        (self.min / reach, self.max / reach)
    }
}

/// One volume a zone swaps its own ambient inside: the boxes a roofed part of a town is lit by
/// rather than by the sky over it.
///
/// The shape codes are the file's own - an `EnvShape` is `Ellipsoid = 1`, `Cuboid = 2`,
/// `Cylinder = 3`, and the composite tests for 1 and 3 and takes everything else as the box.
#[derive(Clone, Copy)]
pub struct Volume {
    /// Takes a place in front of the camera into the volume's own space, where it stands as the unit
    /// shape its kind names.
    pub into: Mat4,
    /// How sharply it takes over across each face, in units of its own half extent. The composite
    /// weighs a pixel by this against how far into the volume it stands and drops the volume where
    /// that reaches nought.
    pub fade: Vec3,
    pub shape: f32,
    /// The light inside, which is another place's harmonics rather than the zone's own.
    pub light: [Vec4; 3],
    pub scale: f32,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            into: Mat4::IDENTITY,
            fade: Vec3::ONE,
            shape: 0.0,
            light: [Vec4::ZERO; 3],
            scale: 1.0,
        }
    }
}

/// The light a place stands in, as `g_AmbientParamArray` holds one entry of it.
///
/// Each set of harmonics is three rows a shader dots against a normal and a one. A zone states its
/// own per time of day in the `.amb` its `EnvLocation` names, the sky's own come out of
/// `skylight.amb`, `scale` is what a zone's `.envb` calls `ambient_light_scale` and the fade's floor
/// what it calls `parameter_1`.
#[derive(Clone)]
pub struct Ambient {
    pub sky: [Vec4; 3],
    /// What the sky's harmonics are taken back up by.
    pub sky_scale: f32,
    pub light: [Vec4; 3],
    pub scale: f32,
    /// How the ambient fades with the depth of the pixel. The composite squares `x * depth + y`
    /// keeping its sign, then clamps that between the floor and one, so a positive ramp leaves the
    /// ambient alone and only the floor bites.
    pub fade: Vec3,
    /// What a sampled reflection is scaled and biased by, and which of the two reflected terms the
    /// frame takes: nought the one weighed by occlusion and `3 * pi / 4`, one the term raw.
    pub reflection: Vec3,
    /// Which cube of the reflection array a place reflects, which the composite reaches as the
    /// slice `0.1 + this`.
    pub capture: f32,
    /// What the ambient is mixed toward, and how far that mix reaches.
    pub haze: Vec4,
    /// The places inside the zone that light themselves, past the one lighting the whole of it.
    pub volumes: std::sync::Arc<[Volume]>,
}

impl Default for Ambient {
    fn default() -> Self {
        Self {
            // A place with no zone around it still has to state a sky, since that is the only thing
            // a smooth surface has to reflect. Brighter overhead than underfoot, and this viewer's
            // own: nothing on disk states what a model out of any zone stands in.
            sky: [
                Vec4::new(0.0, 0.12, 0.0, 0.26),
                Vec4::new(0.0, 0.13, 0.0, 0.28),
                Vec4::new(0.0, 0.16, 0.0, 0.33),
            ],
            sky_scale: 1.0,
            light: [Vec4::new(0.0, 0.0, 0.0, 0.12); 3],
            scale: 1.0,
            fade: Vec3::new(0.0, 1.0, 0.0),
            reflection: Vec3::X,
            capture: 0.0,
            haze: Vec4::W,
            volumes: std::sync::Arc::from([] as [Volume; 0]),
        }
    }
}

impl Scene {
    /// The planes the projection was built with, which is the only scale the frame states: the
    /// viewer cuts them to the model's own bounding sphere.
    fn planes(&self) -> (f32, f32) {
        let (z, w) = (self.projection.z_axis.z, self.projection.w_axis.z);
        (w / z, w / (z + 1.0))
    }
}

impl Ambient {
    /// The harmonics of one channel as the row a normal is dotted against, from the nine
    /// coefficients a file states. The file runs constant, `y`, `z`, `x`, and the row is dotted
    /// against `(normal, 1)`, so the three linear terms are the first three lanes and the constant
    /// the last. What the shader does with the six second-order terms it never reads.
    pub fn row(coefficients: &[f32; 9]) -> Vec4 {
        // Convolved with a cosine lobe rather than handed over raw. Harmonics state the light
        // arriving from each direction; what a surface takes is that gathered over the hemisphere it
        // faces, which weighs the constant term by `pi * Y00` and the three linear ones by
        // `(2pi/3) * Y1m`. Handing them over unscaled leaves the ambient too dark and too flat, by
        // these two factors and by the `2/sqrt(3)` between them.
        const CONSTANT: f32 = 0.886_226_9;
        const LINEAR: f32 = 1.023_326_7;
        Vec4::new(
            coefficients[3] * LINEAR,
            coefficients[1] * LINEAR,
            coefficients[2] * LINEAR,
            coefficients[0] * CONSTANT,
        )
    }
}

/// What the viewer draws with, past what the files decide. Most of these are constants a pass of the
/// post chain reads and no file states: the buffers behind them report no member names and no
/// defaults at all, so what the sliders open at is a guess and nothing more.
#[derive(Clone, Copy, PartialEq)]
pub struct Look {
    /// The longest edge a model's textures are decoded to, or the file's own where nothing caps it.
    /// Not a shader constant: it decides which mipmap is fetched.
    pub detail: Option<u16>,
    pub antialias: bool,
    /// `fxaaQualitySubpix`, at FXAA 3.11's own default. The shader takes one less it, so the slider
    /// runs the way the published constant does rather than the way the buffer holds it.
    pub subpix: f32,
    /// `fxaaQualityEdgeThreshold`, likewise. The threshold it is held against is the `0.0833` the
    /// shader carries as a literal, which is what identifies the pass as stock FXAA.
    pub edge: f32,
    pub occlude: bool,
    /// Which of [`OCCLUDERS`] runs.
    pub quality: usize,
    /// What the tap offsets the pass carries are scaled by, in texels of what it reads.
    pub radius: f32,
    /// How steeply a valley counts, as the fall in depth over the distance to it. The one lane of
    /// the three below that is a ratio rather than a length.
    pub accept: f32,
    /// The fall past which two samples are no longer one surface, the distance under which
    /// occlusion fades out, and the distance past which a pixel is left alone. The first two are
    /// fractions of the depth the model itself spans, the last of the far plane: the frame is cut to
    /// the model's own bounding sphere, so a length stated in the world would mean something
    /// different for every file opened.
    pub reject: f32,
    pub fade: f32,
    pub distance: f32,
    /// How far along its own normal a sample is pushed before it is compared, likewise a fraction of
    /// that span.
    pub bias: f32,
    pub intensity: f32,
    /// The exponent the occlusion is raised to. The pass also multiplies by it a second time, which
    /// is the file's own arithmetic and not a reading of it.
    pub power: f32,
}

/// The occlusion values are a guess. Nothing states them: the buffer behind them reports no member
/// names, no defaults, and no units.
impl Default for Look {
    fn default() -> Self {
        Self {
            detail: None,
            antialias: true,
            subpix: 0.75,
            edge: 0.166,
            occlude: true,
            quality: 6,
            radius: 2.0,
            accept: 150.0,
            reject: 0.05,
            fade: 0.02,
            distance: 1.0,
            bias: 0.02,
            intensity: 3.0,
            power: 1.0,
        }
    }
}

impl Look {
    pub fn occluder(&self) -> String {
        let at = self.quality.min(OCCLUDERS.len() - 1) + 1;
        format!("shader/sm5/posteffect/SSAO{at}.shcd")
    }
}

/// What the environment's tone mapping set states at the weather and time the frame stands at, and
/// what the chain reading it answered for the frame before this one. Every field but the last two is
/// the file's own.
#[derive(Clone, Copy)]
pub struct Exposure {
    pub min: f32,
    pub max: f32,
    /// Per second: the buffer holds it scaled by how long a frame took.
    pub rate: f32,
    /// The buffer holds its square.
    pub key: f32,
    /// How much of the curve reaches the frame, and how far the curve bends toward the exposure.
    pub strength: f32,
    pub shoulder: f32,
    pub step: f32,
    /// The exposure `AdaptLum` last answered, which is what the passes here read the frame under.
    pub adapted: f32,
}

/// What the sky pass reads itself against: the hour, which places the sun and picks the slice of the
/// volume the frame stands under, and that volume's own width and height, which fix the coordinates
/// it is read at.
#[derive(Clone, Copy)]
pub struct Sky {
    /// Seconds since midnight.
    pub time: f32,
    /// How far the sun's circle leans, in degrees, which is the zone's own.
    pub tilt: f32,
    pub size: (f32, f32),
    /// Slices the volume holds, which is one an hour however deep it is: the tail of a deeper one
    /// repeats its start so that reading between them wraps past midnight.
    pub depth: f32,
}

/// Which way the sun comes from at an hour of the day, in world space. It rises due `+x` at six and
/// stands a quarter turn up at noon, and the sky, the clouds and every light that follows it read
/// the same one.
///
/// The circle it runs on leans, and by how much the **zone** states in its own level file. Five
/// captures of four zones, each exact to four decimal places against what the file holds.
pub fn sun(time: f32, tilt: f32) -> Vec3 {
    let turned = (time / 3600.0 - 6.0) * std::f32::consts::FRAC_PI_2 / 6.0;
    let (flat, up) = tilt.to_radians().sin_cos();
    Vec3::new(turned.cos(), turned.sin() * up, turned.sin() * flat)
}

/// What a place with no level file of its own stands under, which is what most zones state.
pub const TILT: f32 = 30.0;

/// How far about the point it looks at the sun's own depth map reaches. Not the game's own split:
/// its first cascade is under five units and it draws five of them, where this draws one, so the
/// box has to cover what one map can and still hold enough texels to a unit to read as an edge.
pub const SHADOW_REACH: f32 = 64.0;

/// Where the sun stands to draw the scene's depth, as a view and an orthographic projection about
/// `focus`. The projection matches the one the frame is drawn with in handing back a nought-to-one
/// depth, which is what the translator's own fixup leaves in the buffer.
pub fn shadow_camera(light: Vec3, view: Mat4) -> (Mat4, Mat4) {
    // Taken from the frame's own view rather than passed in, so the pass that draws the map and the
    // matrix that reads it cannot be given different boxes.
    let eye = view.inverse().w_axis.truncate();
    let ahead = -view.row(2).truncate().normalize_or(Vec3::Z);
    let focus = eye + ahead * SHADOW_REACH * 0.5;
    let toward = light.normalize_or(Vec3::Y);
    // A light straight overhead leaves the usual up vector parallel to it, and the look-at degenerate.
    let up = match toward.y.abs() > 0.999 {
        true => Vec3::Z,
        false => Vec3::Y,
    };
    let reach = SHADOW_REACH;
    let onto = Mat4::orthographic_rh(-reach, reach, -reach, reach, 0.0, reach * 2.0);
    // Snapped to whole texels of the map. The box follows the camera, so without this every step it
    // takes shifts the grid the depth was rasterised on and an edge crawls across its own surface,
    // which reads as a shadow flickering in and out rather than as one standing still.
    let held = Mat4::look_at_rh(focus + toward * reach, focus, up);
    let texel = 2.0 * reach / SHADOW_MAP as f32;
    let seen = held.transform_point3(focus);
    let drift = Vec3::new(
        seen.x - (seen.x / texel).round() * texel,
        seen.y - (seen.y / texel).round() * texel,
        0.0,
    );
    let focus = focus - held.inverse().transform_vector3(drift);
    (Mat4::look_at_rh(focus + toward * reach, focus, up), onto)
}

/// What the environment's cloud set states at the weather and time the frame stands at: the two
/// colors a cloud is lit and shaded with, and how far up the band reaches.
#[derive(Clone, Copy, PartialEq)]
pub struct Cloud {
    pub diffuse: Vec3,
    pub ambient: Vec3,
    /// The band's own share of the sky, which the two heights the vertex shader works out sum to.
    pub reach: f32,
}

impl Cloud {
    /// The projection to draw them under: the frame's own, with the far plane pushed past the sheet.
    /// A zone clips at how far it loads, which is a few thousand units, and the sheet is forty
    /// thousand across; moving it costs nothing, since both meshes are held at the far plane
    /// whatever depth they really stand at.
    pub fn frustum(scene: &Scene) -> Mat4 {
        let (near, far) = scene.planes();
        let far = far.max(SHEET_SPAN * 3.0);
        let mut out = scene.projection;
        out.z_axis.z = far / (near - far);
        out.w_axis.z = near * far / (near - far);
        out
    }

    /// Where a cloud mesh stands, which is around the camera rather than in the world: the band is a
    /// cylinder of radius two thousand centred on it, and the sheet a paraboloid forty thousand
    /// across. The sheet is snapped to the grid one period of its texture spans, so that it travels
    /// with the camera without the texture sliding over it.
    pub fn placement(pass: Pass, eye: Vec3) -> Mat4 {
        let column = |x: f32, y: f32, z: f32| glam::Vec4::new(x, y, z, 0.0);
        match pass {
            Pass::CloudBand => Mat4::from_cols(
                column(BAND_RADIUS, 0.0, 0.0),
                column(0.0, BAND_RADIUS + eye.y, 0.0),
                column(0.0, 0.0, BAND_RADIUS),
                glam::Vec4::new(eye.x, -eye.y, eye.z, 1.0),
            ),
            _ => {
                let period = SHEET_SPAN * 2.0 / SHEET_TILING;
                let snap = |held: f32| (held / period).round() * period;
                Mat4::from_cols(
                    column(SHEET_SPAN, 0.0, 0.0),
                    column(0.0, SHEET_HEIGHT, 0.0),
                    column(0.0, 0.0, SHEET_SPAN),
                    glam::Vec4::new(snap(eye.x), SHEET_RISE, snap(eye.z), 1.0),
                )
            }
        }
    }
}

/// White clouds reaching as far as every weather measured has them reach.
impl Default for Cloud {
    fn default() -> Self {
        Self {
            diffuse: Vec3::ONE,
            ambient: Vec3::ONE,
            reach: 0.9,
        }
    }
}

/// The shape every sky the game ships is but one, so a frame whose volume has not arrived still
/// addresses it the way the rest will be addressed.
impl Default for Sky {
    fn default() -> Self {
        Self {
            time: 0.0,
            tilt: TILT,
            size: (8.0, 32.0),
            depth: 24.0,
        }
    }
}

/// What the environment's vertical fog set states at the weather and time the frame stands at. Every
/// field is the file's own, and the two rates are stated per thousand and per seven thousand four
/// hundred units rather than per one.
#[derive(Clone, Copy, PartialEq)]
pub struct Fog {
    /// What a fogged pixel is dragged toward, before the exposure divides it.
    pub color: Vec3,
    /// How opaque it ever gets, which is that color's own alpha.
    pub cap: f32,
    /// How fast the opacity climbs past `start`, and the sky's share past `fade`.
    pub rate: f32,
    pub blend: f32,
    pub start: f32,
    pub fade: f32,

    /// Whether the zone runs the near haze at all, and how far in front of the camera it begins.
    pub haze: f32,
    pub near: f32,
    /// The two layers the haze sums, each thinning away from a height of its own: how fast it
    /// thins, how thick it is at that height, and where that height sits.
    pub layers: [Vec3; 2],
    /// How much of the frame the haze is ever allowed to leave standing.
    pub clear: f32,

    /// What the sun adds to a pixel looking into the haze: its color, how strong, how tightly it
    /// gathers around the sun, and how far out it starts.
    pub glow: Vec3,
    pub glow_strength: f32,
    pub glow_sharpness: f32,
    pub glow_start: f32,
}

/// No fog at all, which is a frame the weather states none for.
impl Default for Fog {
    fn default() -> Self {
        Self {
            color: Vec3::ZERO,
            cap: 0.0,
            rate: 0.0,
            blend: 0.0,
            start: 0.0,
            fade: 0.0,
            haze: 0.0,
            near: 0.0,
            layers: [Vec3::ZERO; 2],
            clear: 0.0,
            glow: Vec3::ZERO,
            glow_strength: 0.0,
            glow_sharpness: 0.0,
            glow_start: 0.0,
        }
    }
}

impl Fog {
    /// Where the table stops changing, which is the later of the two channels' own saturations. One
    /// climbing at nothing never saturates and stands for nothing here.
    pub fn far(&self) -> f32 {
        let held = |from: f32, over: f32, rate: f32| (rate > 0.0).then(|| from + over / rate);
        held(self.start, self.cap, self.rate)
            .into_iter()
            .chain(held(self.fade, 1.0, self.blend))
            .fold(self.start, f32::max)
    }

    /// The table itself, two channels a texel: how opaque the fog is at that distance, and how far
    /// the color it mixes toward has gone from the fog's own to the sky's. The first is linear under
    /// its cap and the second the square of a linear ramp, which is what the game's own tables are.
    pub fn table(&self) -> Vec<f32> {
        let last = FOG_TABLE as f32 - 1.0;
        let span = self.far() - self.start;
        (0..FOG_TABLE)
            .flat_map(|at| {
                let z = self.start + span * at as f32 / last;
                let toward = ((z - self.fade) * self.blend).clamp(0.0, 1.0);
                [
                    ((z - self.start) * self.rate).clamp(0.0, self.cap),
                    toward * toward,
                ]
            })
            .collect()
    }
}

/// What leaves the frame as the composite resolved it: no exposure, and a curve of no strength.
impl Default for Exposure {
    fn default() -> Self {
        Self {
            min: 1.0,
            max: 1.0,
            rate: 0.0,
            key: 1.0,
            strength: 0.0,
            shoulder: 0.0,
            step: 0.0,
            adapted: 1.0,
        }
    }
}

/// What the engine decides rather than the files. Everything a constant buffer holds that is not the
/// material's own comes from here, so a field that has to be reconstructed is reconstructed once.
#[derive(Clone)]
pub struct Scene {
    pub view: Mat4,
    pub projection: Mat4,
    pub model: Mat4,
    /// The frame in pixels, which is what a screen-wide pass turns a fragment into a texel with.
    pub size: (f32, f32),
    /// Which way the sun comes from, in world space.
    pub light: Vec3,
    /// The light a lamp pass is drawing.
    pub lamp: Lamp,
    pub diffuse: Vec3,
    pub specular: Vec3,
    /// What the composite lights a surface with where no light reaches it.
    pub ambient: Ambient,
    /// What the passes past the composite are run with.
    pub look: Look,
    pub exposure: Exposure,
    pub sky: Sky,
    pub fog: Fog,
    pub cloud: Cloud,
    /// The colours the character was made with.
    pub customize: Customize,
    /// Seconds since the viewer opened, which is what every wave and every leaf is a sine of.
    pub clock: f32,
    pub wind: Wind,
}

/// What a leaf is swayed by, which is all three registers `g_WavingParam` holds. The heading and the
/// reach come out of the weather's wind set; the rate does not, since the shader takes its whole
/// phase from the engine and no file states how fast one sway runs. A mesh weights the reach by its
/// own stream, which reaches a tenth at most, so the stated strength is already in world units.
#[derive(Clone, Copy)]
pub struct Wind {
    /// Which way a leaf leans, in world space.
    pub heading: Vec3,
    /// How far it leans at the far end of one sway, in world units.
    pub reach: f32,
    /// Radians of phase a second.
    pub rate: f32,
}

/// What a lone model is shown under, since nothing outside a zone names an environment to take a
/// wind out of. The panel spells all three out.
impl Default for Wind {
    fn default() -> Self {
        Self {
            heading: Vec3::new(0.92, 0.0, 0.38),
            reach: 4.0,
            rate: 1.6,
        }
    }
}

/// What character creation decides, which no file a model names holds: each is what an albedo is
/// multiplied by or mixed toward. White leaves a texture's own colour where it is, which is what a
/// model outside the character tab is drawn with.
#[derive(Clone, Copy)]
pub struct Customize {
    pub skin: [f32; 4],
    /// A lip tint, whose alpha is the weight it is mixed at rather than an opacity.
    pub lip: [f32; 4],
    pub hair: [f32; 4],
    /// A hair highlight, drawn only where its alpha says to.
    pub highlight: [f32; 4],
    pub left_eye: [f32; 4],
    pub right_eye: [f32; 4],
    /// What a face paint or a limbal ring is tinted with.
    pub option: [f32; 3],
}

impl Default for Customize {
    fn default() -> Self {
        Self {
            skin: [1.0; 4],
            lip: [1.0, 1.0, 1.0, 0.0],
            hair: [1.0; 4],
            highlight: [1.0, 1.0, 1.0, 0.0],
            left_eye: [1.0; 4],
            right_eye: [1.0; 4],
            option: [1.0; 3],
        }
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            view: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            model: Mat4::IDENTITY,
            size: (1.0, 1.0),
            light: Vec3::Y,
            lamp: Lamp::default(),
            diffuse: Vec3::ONE,
            specular: Vec3::ONE,
            ambient: Ambient::default(),
            look: Look::default(),
            exposure: Exposure::default(),
            sky: Sky::default(),
            fog: Fog::default(),
            cloud: Cloud::default(),
            customize: Customize::default(),
            clock: 0.0,
            wind: Wind::default(),
        }
    }
}

/// Everything one draw of one material needs, worked out off the files rather than held on the card.
pub struct Program {
    pub vertex: String,
    pub fragment: String,
    pub attributes: Vec<Attribute>,
    pub textures: Vec<Texture>,
    pub buffers: Vec<Buffer>,
    pub structured: Vec<Structured>,
    /// Every target the shader declares, in register order.
    pub outputs: Vec<u32>,
    /// The targets this reading writes, in attachment order: one page of `outputs`.
    pub targets: Vec<u32>,
    /// What each of `outputs` is called.
    pub names: Vec<String>,
    /// Which pass this is, since two packages read the same buffer differently: a sun's attenuation
    /// fades with depth and a lamp's with the square of the distance.
    pub pass: Pass,
}

/// The positional polynomial a package identifies a node by, applied over each group of keys and
/// then over the four results.
fn selector(keys: &[u32]) -> u32 {
    let (mut out, mut mul) = (0u32, 1u32);
    for key in keys {
        out = out.wrapping_add(key.wrapping_mul(mul));
        mul = mul.wrapping_mul(31);
    }
    out
}

/// What a group of keys resolves to: the draw's own value where it sets the category, the material's
/// where it names it, and the package's default otherwise.
fn values(keys: &[shpk::Key], material: &[mtrl::ShaderKey], set: &[(u32, u32)]) -> Vec<u32> {
    keys.iter()
        .map(|key| {
            set.iter()
                .find(|(id, _)| *id == key.id())
                .map(|(_, value)| *value)
                .or_else(|| {
                    material
                        .iter()
                        .find(|held| held.category() == key.id())
                        .map(mtrl::ShaderKey::value)
                })
                .unwrap_or_else(|| key.default_value())
        })
        .collect()
}

/// The shaders this material would draw the pass with, as indices into the package's own list.
fn pair(
    package: &ShaderPackage,
    material: &[mtrl::ShaderKey],
    set: &[(u32, u32)],
    pass: u32,
    technique: u32,
    subview: u32,
) -> Option<(u32, u32)> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, material, set)))
    .collect();
    parts.push(selector(&[technique, subview]));
    let id = selector(&parts);

    // Lookup is by node id, falling back to the alias table: skin and hair only resolve through it.
    let node = package
        .nodes()
        .iter()
        .find(|node| node.id() == id)
        .or_else(|| {
            let alias = package
                .aliases()
                .iter()
                .find(|alias| alias.selector() == id)?;
            package.nodes().get(alias.node() as usize)
        })?;
    let held = node.passes().iter().find(|held| held.id() == pass)?;
    if held.vertex() == shpk::NONE || held.pixel() == shpk::NONE {
        return None;
    }
    // A pass names a shader by its index within its own stage, not within the whole list.
    let base = |want: Stage| {
        package
            .shaders()
            .iter()
            .take_while(|shader| shader.stage() != want)
            .count() as u32
    };
    Some((
        base(Stage::Vertex) + held.vertex(),
        base(Stage::Pixel) + held.pixel(),
    ))
}

/// One shader's blob, and the program the disassembler read out of it.
fn program<'a>(
    package: &ShaderPackage,
    bytes: &'a [u8],
    index: u32,
) -> Option<(dxbc::shex::Program, &'a [u8])> {
    let shader = package.shaders().get(index as usize)?;
    let start = package.blobs_offset() + usize::try_from(shader.blob_offset()).ok()?;
    let end = start.checked_add(usize::try_from(shader.blob_size()).ok()?)?;
    let blob = bytes.get(start..end)?;
    Some((shex(blob)?, blob))
}

/// The program the disassembler reads out of a blob.
fn shex(blob: &[u8]) -> Option<dxbc::shex::Program> {
    dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .find_map(|chunk| match chunk.parse() {
            dxbc::chunks::ChunkData::Shader(program) => Some(program),
            _ => None,
        })
}

/// What a blob's own signature chunks declare its inputs and outputs as. A translation without
/// these emits every one of them as a bare register nothing declared.
fn signatures(blob: &[u8], into: &mut hlsl::Names) {
    use dxbc::chunks::ChunkData;

    for chunk in dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
    {
        let (held, signature) = match chunk.parse() {
            ChunkData::InputSignature(signature) => (&mut into.inputs, signature),
            ChunkData::OutputSignature(signature) => (&mut into.outputs, signature),
            _ => continue,
        };
        for element in &signature.elements {
            held.entry(element.register).or_insert_with(|| {
                hlsl::Semantic::new(
                    &element.semantic_name,
                    element.semantic_index,
                    element.component_type,
                    element.mask,
                )
            });
        }
    }
}

/// What this shader's registers are called, and what its signatures declare.
fn names(package: &ShaderPackage, index: u32, blob: &[u8]) -> hlsl::Names {
    let mut names = hlsl::Names::default();
    let Some(shader) = package.shaders().get(index as usize) else {
        return names;
    };
    let named = |resource: &shpk::Resource| {
        package
            .name(resource)
            .map(str::to_owned)
            .or_else(|| shaders::names::resolve(resource.id()).map(str::to_owned))
    };
    for resource in shader.textures() {
        if let Some(name) = named(resource) {
            names.textures.insert(resource.slot(), name);
        }
    }
    for resource in shader.samplers() {
        if let Some(name) = named(resource) {
            names.samplers.insert(resource.slot(), name);
        }
    }
    for resource in shader.constants() {
        if let Some(name) = named(resource) {
            names
                .constants
                .insert(resource.slot(), hlsl::Buffer::new(name, Vec::new()));
        }
    }
    signatures(blob, &mut names);
    names
}

/// The buffer layouts a blob's own reflection describes, by name.
fn layouts(blob: &[u8], into: &mut HashMap<String, Vec<hlsl::layout::Member>>) {
    for chunk in dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
    {
        if let dxbc::chunks::ChunkData::Rdef(rdef) = chunk.parse() {
            for buffer in &rdef.constant_buffers {
                into.entry(buffer.name.to_string())
                    .or_insert_with(|| hlsl::layout::members(buffer));
            }
        }
    }
}

/// Which field of a vertex a semantic reads from. One the mesh has nothing for is left to the
/// generic attribute, which the draw sets to something the shader can work with.
fn field(semantic: &str) -> Option<Field> {
    Some(match semantic.to_ascii_uppercase().as_str() {
        "POSITION" => Field::Position,
        "NORMAL" => Field::Normal,
        "BINORMAL" => Field::Tangent,
        "TANGENT" => Field::Bitangent,
        "TEXCOORD" => Field::Uv,
        "TEXCOORD1" => Field::Uv1,
        "COLOR" => Field::Color,
        "COLOR1" => Field::Color1,
        "BLENDWEIGHT" => Field::Weights,
        "BLENDINDICES" => Field::Bones,
        _ => return None,
    })
}

/// What target a sampler the translation declared has to be bound at.
fn kind(declaration: &str) -> Kind {
    match declaration {
        "sampler2DArray" | "sampler2DArrayShadow" => Kind::Array,
        "sampler3D" => Kind::Volume,
        "samplerCube" | "samplerCubeShadow" => Kind::Cube,
        _ => Kind::Plane,
    }
}

/// The parameter buffer as this draw sees it: the package's own defaults, with the material's
/// constants written over the spans the package says they occupy.
fn parameters(package: &ShaderPackage, material: &mtrl::Material) -> Vec<u8> {
    let mut out = vec![0u8; package.param_buffer_size() as usize];
    let mut put = |at: usize, values: &[f32]| {
        for (lane, value) in values.iter().enumerate() {
            let offset = at + lane * 4;
            if offset + 4 <= out.len() {
                out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
    };
    put(0, package.param_defaults());
    for param in package.material_params() {
        let Some(values) = material
            .constants()
            .iter()
            .find(|held| held.id() == param.id())
            .and_then(|constant| material.constant_values(constant))
        else {
            continue;
        };
        let lanes = param.byte_size() as usize / 4;
        put(
            param.byte_offset() as usize,
            &values[..lanes.min(values.len())],
        );
    }
    out
}

impl Program {
    /// Translates the pair this material would draw with. `target` names a G-buffer channel; the
    /// page holding it is what the fragment shader is emitted with, so a context with four draw
    /// buffers reaches the fifth target through a reading of its own.
    pub fn build(
        bytes: &[u8],
        material: &Material,
        set: &[(u32, u32)],
        pass: Pass,
        subview: u32,
        target: usize,
        attachments: usize,
    ) -> Result<Self, String> {
        let package = ShaderPackage::parse(bytes).map_err(|why| why.to_string())?;
        let held = material.held();
        let technique = package.technique_subview()[0];
        let (vs, ps) = pair(&package, held.shader_keys(), set, pass.id(), technique, subview)
            .ok_or("this material's keys reach no such pass")?;
        Self::assemble(
            &package,
            bytes,
            (vs, ps),
            Some(held),
            pass,
            target,
            attachments,
        )
    }

    /// Translates a pass of a package no material names: the ones that light and resolve what the
    /// G-buffer holds, which the engine runs over the whole frame rather than over one draw.
    pub fn screen(
        bytes: &[u8],
        pass: Pass,
        attachments: usize,
        keys: &[(u32, u32)],
    ) -> Result<Self, String> {
        let package = ShaderPackage::parse(bytes).map_err(|why| why.to_string())?;
        // A key a package does not declare is never looked up, so one set serves every package here.
        // `keys` is what one package alone asks for, since a key two of them declare would move both.
        let mut set = vec![
            // The shadowed variant, which reads the mask the resolve leaves. Only
            // `directionallighting` declares this key at all, so no other package here moves with
            // it, and the blend it does is `min(mask, fade^2 * (w - mask) + mask)`: at the `w` of
            // one that buffer states, the second term never falls below the mask, so what lands is
            // the mask whatever the cloud term holds.
            (GET_DIRECTIONAL_LIGHT, GET_DIRECTIONAL_LIGHT_SHADOW),
            (SPECULAR_LIGHTING, SPECULAR_LIGHTING_ENABLE),
        ];
        set.extend_from_slice(keys);
        let technique = package.technique_subview()[0];
        let (vs, ps) = pair(&package, &[], &set, pass.id(), technique, SUB_VIEW_MAIN)
            .ok_or("this package reaches no such pass")?;
        Self::assemble(&package, bytes, (vs, ps), None, pass, 0, attachments)
    }

    /// Translates one variant of a package the engine draws with geometry it builds itself, where
    /// the technique picks the variant rather than the package's own default: the cloud package
    /// draws its band and its sheet from two of them, over two different meshes.
    pub fn cloud(bytes: &[u8], pass: Pass, attachments: usize) -> Result<Self, String> {
        let package = ShaderPackage::parse(bytes).map_err(|why| why.to_string())?;
        let technique = match pass {
            Pass::CloudBand => CLOUD_BAND,
            _ => CLOUD_SHEET,
        };
        let subview = package.technique_subview()[1];
        let (vs, ps) = pair(&package, &[], &[], pass.id(), technique, subview)
            .ok_or("the cloud package holds no such technique")?;
        Self::assemble(&package, bytes, (vs, ps), None, pass, 0, attachments)
    }

    /// Translates one member of the game's post chain. A `.shcd` holds one shader and no node table,
    /// so the file is the variant and there is nothing to select; what it wants is a screen-wide
    /// draw of the vertex shader given, and a frame in the range a screen holds, since the pass that
    /// grades one saturates what it reads before it reads its table. The path is taken because two
    /// members read the same buffer as different things.
    pub fn posteffect(path: &str, bytes: &[u8], vertex: &str) -> Result<Self, String> {
        let code = shcd::ShaderCode::parse(bytes).map_err(|why| why.to_string())?;
        let blob = bytes
            .get(code.blob_offset()..code.blob_offset() + code.blob_size())
            .ok_or("the shader's bytecode runs past the file")?;
        let fragment = shex(blob).ok_or("no shader in the blob")?;

        let mut names = hlsl::Names::default();
        for (resources, into) in [
            (code.textures(), &mut names.textures),
            (code.samplers(), &mut names.samplers),
        ] {
            for resource in resources {
                if let Some(name) = code.name(resource) {
                    into.insert(resource.slot(), name.to_owned());
                }
            }
        }
        for resource in code.constants() {
            if let Some(name) = code.name(resource) {
                names.constants.insert(
                    resource.slot(),
                    hlsl::Buffer::new(name.to_owned(), Vec::new()),
                );
            }
        }
        signatures(blob, &mut names);

        let outputs: Vec<u32> = names
            .outputs
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let extents = hlsl::glsl::extents(&fragment, &names);
        let options = hlsl::glsl::Options {
            targets: outputs.clone(),
            extents: extents.clone(),
        };

        let declared = hlsl::glsl::declarations(&fragment);
        let textures = hlsl::glsl::textures(&fragment, &names)
            .into_iter()
            .filter_map(|(slot, _, name)| {
                let resource = code.textures().iter().find(|held| held.slot() == slot)?;
                Some(Texture {
                    name,
                    id: resource.id(),
                    kind: kind(declared.get(&slot).copied().unwrap_or_default()),
                })
            })
            .collect();
        let buffers = extents
            .into_iter()
            .map(|(name, registers)| Buffer {
                fixed: (name == TONE_MAP_PARAM && path == TONE_ADJUST).then(|| {
                    TONE_MAP
                        .iter()
                        .flat_map(|held| held.to_le_bytes())
                        .collect()
                }),
                name,
                members: Vec::new(),
                registers,
            })
            .collect();

        Ok(Self {
            vertex: vertex.to_owned(),
            fragment: hlsl::glsl(&fragment, &names, hlsl::Reading::Plain, &options)
                .lines
                .join("\n"),
            attributes: Vec::new(),
            textures,
            buffers,
            structured: Vec::new(),
            names: outputs.iter().map(|at| format!("SV_Target{at}")).collect(),
            targets: outputs.clone(),
            outputs,
            pass: Pass::Composite,
        })
    }

    fn assemble(
        package: &ShaderPackage,
        bytes: &[u8],
        (vs, ps): (u32, u32),
        material: Option<&mtrl::Material>,
        pass: Pass,
        target: usize,
        attachments: usize,
    ) -> Result<Self, String> {
        let (vertex, vs_blob) =
            program(package, bytes, vs).ok_or("no vertex shader in the blob")?;
        let (fragment, ps_blob) =
            program(package, bytes, ps).ok_or("no pixel shader in the blob")?;
        let vs_names = names(package, vs, vs_blob);
        let ps_names = names(package, ps, ps_blob);
        let mut described = HashMap::new();
        layouts(vs_blob, &mut described);
        layouts(ps_blob, &mut described);

        // A uniform block has to be spelled identically in both stages or the program will not link,
        // and the two disagree on the extent of a shared buffer more often than not.
        let mut extents = hlsl::glsl::extents(&vertex, &vs_names);
        for (name, registers) in hlsl::glsl::extents(&fragment, &ps_names) {
            let held = extents.entry(name).or_insert(0);
            *held = (*held).max(registers);
        }

        let outputs: Vec<u32> = ps_names
            .outputs
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let attachments = attachments.max(1);
        let targets: Vec<u32> = outputs
            .chunks(attachments)
            .nth(target / attachments)
            .unwrap_or_default()
            .to_vec();

        let vs_options = hlsl::glsl::Options {
            targets: Vec::new(),
            extents: extents.clone(),
        };
        let ps_options = hlsl::glsl::Options {
            targets: targets.clone(),
            extents,
        };
        let read = |program, names, options| {
            hlsl::glsl(program, names, hlsl::Reading::Plain, options)
                .lines
                .join("\n")
        };

        let mut attributes: Vec<Attribute> = vs_names
            .inputs
            .iter()
            .filter_map(|(register, entry)| {
                Some(Attribute {
                    location: *register,
                    field: field(&entry.name)?,
                    components: match entry.kind.as_str() {
                        held if held.starts_with("uint") => Components::Unsigned,
                        held if held.starts_with("int") => Components::Signed,
                        _ => Components::Float,
                    },
                })
            })
            .collect();
        attributes.sort_by_key(|held| held.location);

        // Bound by the package's own resource id rather than by slot: the same name sits at
        // different slots across variants of one package.
        let mut textures: Vec<Texture> = Vec::new();
        for (shader, program, names) in [(vs, &vertex, &vs_names), (ps, &fragment, &ps_names)] {
            let resources = package
                .shaders()
                .get(shader as usize)
                .map(shpk::Shader::textures)
                .unwrap_or_default();
            let declared = hlsl::glsl::declarations(program);
            for (slot, _, name) in hlsl::glsl::textures(program, names) {
                let Some(resource) = resources.iter().find(|held| held.slot() == slot) else {
                    continue;
                };
                if textures.iter().all(|held| held.name != name) {
                    textures.push(Texture {
                        name,
                        id: resource.id(),
                        kind: kind(declared.get(&slot).copied().unwrap_or_default()),
                    });
                }
            }
        }

        let parameters = material.map(|held| parameters(package, held));
        let mut structured: Vec<Structured> = Vec::new();
        let mut buffers: Vec<Buffer> = Vec::new();
        for (program, names) in [(&vertex, &vs_names), (&fragment, &ps_names)] {
            for (name, stride) in hlsl::glsl::buffers(program, names) {
                if structured.iter().all(|held| held.name != name) {
                    structured.push(Structured {
                        name,
                        stride: stride as usize,
                    });
                }
            }
            for (name, registers) in hlsl::glsl::extents(program, names) {
                if buffers.iter().any(|held| held.name == name) {
                    continue;
                }
                let fixed = (name == "g_MaterialParameter")
                    .then(|| parameters.clone())
                    .flatten();
                let members = match described.get(&name) {
                    Some(held) => held.clone(),
                    None if name == INSTANCE => instance_fields(),
                    None => Vec::new(),
                };
                buffers.push(Buffer {
                    members,
                    name,
                    registers,
                    fixed,
                });
            }
        }

        let names = outputs
            .iter()
            .map(|register| {
                ps_names
                    .outputs
                    .get(register)
                    .map_or_else(|| format!("SV_Target{register}"), |held| held.name.clone())
            })
            .collect();

        Ok(Self {
            vertex: read(&vertex, &vs_names, &vs_options),
            fragment: read(&fragment, &ps_names, &ps_options),
            attributes,
            textures,
            buffers,
            structured,
            outputs,
            targets,
            names,
            pass,
        })
    }

    /// Where in this reading's attachments the wanted target landed.
    pub fn attachment(&self, target: usize) -> Option<usize> {
        let register = self.outputs.get(target)?;
        self.targets.iter().position(|held| held == register)
    }

    /// The buffer this pass reads one record of per object drawn, and how many records it holds.
    pub fn instancing(&self) -> Option<(&Buffer, usize)> {
        self.buffers
            .iter()
            .map(|buffer| (buffer, buffer.instances()))
            .find(|(_, count)| *count > 1)
    }

    /// How many objects one draw of this pass covers. A package with no instancing buffer draws one.
    pub fn batch(&self) -> usize {
        self.instancing().map_or(1, |(_, count)| count)
    }
}

impl Buffer {
    /// Bytes of one record, where the reflection describes one and the buffer holds many.
    fn stride(&self) -> u32 {
        self.members
            .iter()
            .map(|member| member.offset + member.size)
            .max()
            .unwrap_or(0)
            .max(16)
            .div_ceil(16)
            * 16
    }

    /// How many objects one draw covers, which is the instancing buffer's own extent over the
    /// record the reflection describes.
    pub fn instances(&self) -> usize {
        match self.name == INSTANCING {
            true => (self.registers * 16 / self.stride()).max(1) as usize,
            false => 1,
        }
    }

    /// The bytes this buffer holds, filled by field name off the reflection. What the files decide
    /// is worked out once; everything else is the camera, the objects being drawn and the light this
    /// pass carries, and whatever nothing names stays zero.
    pub fn fill(&self, scene: &Scene, pass: Pass, instances: &[Instance]) -> Vec<u8> {
        let Scene {
            view,
            projection,
            model,
            size,
            clock,
            ..
        } = *scene;
        let span = self
            .members
            .iter()
            .map(|member| member.offset + member.size)
            .max()
            .unwrap_or(0)
            .max(self.registers * 16)
            .max(16);
        let mut out = vec![0u8; span.div_ceil(16) as usize * 16];
        if let Some(fixed) = &self.fixed {
            let end = fixed.len().min(out.len());
            out[..end].copy_from_slice(&fixed[..end]);
            return out;
        }

        // A matrix reads as its rows, since a register of the buffer is a row and the machine takes
        // a dot product against one.
        let rows = |matrix: Mat4, count: usize| -> Vec<f32> {
            matrix.transpose().to_cols_array()[..count * 4].to_vec()
        };
        // Aimed at one buffer, since the same name means different things in two of them: a light's
        // diffuse color is also what skin under a stocking is multiplied by.
        let mut put = |buffer: &str, name: &str, values: Vec<f32>| {
            if self.name != buffer {
                return;
            }
            let Some(member) = self.members.iter().find(|held| held.name == name) else {
                return;
            };
            // A field the reflection calls a dword is read back through the bit pattern, so a whole
            // number goes in as one rather than as the float that reads the same.
            let whole = member.kind == "dword" || member.kind.starts_with("uint");
            // The same name is declared at different extents across packages, so a write is cut to
            // the one this buffer states: anything past it is the next field along.
            let end = out.len().min((member.offset + member.size) as usize);
            for (at, value) in values.iter().enumerate() {
                let offset = member.offset as usize + at * 4;
                let bits = match whole {
                    true => (*value as u32).to_le_bytes(),
                    false => value.to_le_bytes(),
                };
                if offset + 4 <= end {
                    out[offset..offset + 4].copy_from_slice(&bits);
                }
            }
        };
        if self.name == INSTANCING {
            self.instancing(scene, instances, &mut out);
            return out;
        }
        if self.name == "g_AmbientParamArray" {
            ambient(&scene.ambient, glam::Mat3::from_mat4(view), view, &mut out);
            return out;
        }
        if self.name == "g_AmbientParam" {
            entry(&scene.ambient, glam::Mat3::from_mat4(view), &mut out, 0);
            return out;
        }
        if self.name == "g_BGAmbientParameter" {
            write(&mut out, 0, &scene.ambient.haze.to_array());
            return out;
        }
        if self.name == VIEWPORT_PARAM {
            write(&mut out, 0, &[1.0; 4]);
            return out;
        }
        // A whole matrix, one register per row, for a buffer the reflection gives no members.
        let write_rows = |out: &mut Vec<u8>, held: &[f32]| {
            for (at, row) in held.chunks(4).enumerate() {
                write(out, at, row);
            }
        };
        let exposure = scene.exposure;
        // The exposure the last frame settled on, which is what this one is measured and read under.
        let adapted = exposure.adapted.max(f32::EPSILON);
        if self.name == ADAPT_LUM_PARAM {
            // The rate is stated per second and the buffer wants what one frame moves by. The pass
            // reads a weight of one and over as a frame to start again from rather than carry on
            // through, which is what a step long enough to overshoot should do anyway.
            let step = (exposure.rate * exposure.step).min(1.0);
            write(
                &mut out,
                0,
                &[exposure.min, exposure.max, step, exposure.key * exposure.key],
            );
            return out;
        }
        if self.name == PROJECTION_INVERSE {
            write_rows(&mut out, &rows(projection.inverse(), 4));
            return out;
        }
        if self.name == VIEW_INVERSE {
            write_rows(&mut out, &rows(view.inverse(), 4));
            return out;
        }
        if self.name == SKY_PARAM {
            let held = scene.sky;
            let hours = held.time / 3600.0;
            let held_sun = sun(held.time, held.tilt);
            let eye = view.inverse().w_axis;
            let (wide, tall) = held.size;
            write(&mut out, 0, &[eye.x, eye.y, eye.z, 0.0]);
            write(&mut out, 1, &[held_sun.x, held_sun.y, held_sun.z, 0.0]);
            // Cut to the volume's own texel centers, which is what the frame's numbers work out to
            // for the eight by thirty-two every sky but one is.
            write(&mut out, 2, &[0.5, 1.0 - 0.5 / tall, 0.0, 0.0]);
            write(&mut out, 3, &[(1.0 - 1.0 / wide) * 0.5, -(1.0 - 1.0 / tall), 0.0, 0.0]);
            // The hour's own slice, and the exposure the sky is read under with everything else.
            write(&mut out, 4, &[(hours + 0.5) / held.depth, 0.0, 1.0 / adapted, 0.0]);
            // The color the sky is mixed toward, at the weight a real frame carried: nought, so it
            // never reaches the frame. Nothing found states either.
            write(&mut out, 5, &[0.0; 4]);
            return out;
        }
        if self.name == SUN_PARAM {
            let held = scene.sky;
            let (wide, tall) = scene.size;
            // Where the sun stands as this pass reads a pixel's own coordinate, which is a texture
            // one rather than the clip xy the sky takes. The game's runs down the frame and this
            // one up it, so the vertical is the one place the two conventions part.
            let at = projection * view * sun(held.time, held.tilt).extend(0.0);
            let over = at.truncate() / at.w;
            write(&mut out, 0, &[wide / tall, 1.0, over.x * 0.5 + 0.5, over.y * 0.5 + 0.5]);
            write(&mut out, 1, &SUN_RAYS);
            write(&mut out, 2, &SUN_FALLOFF);
            write(&mut out, 3, &SUN_CORE);
            write(&mut out, 4, &SUN_HALO);
            return out;
        }
        if matches!(pass, Pass::CloudBand | Pass::CloudSheet)
            && let Some(register) = [VS_PARAM, PS_PARAM].iter().position(|held| self.name == *held)
        {
            let held = scene.cloud;
            let sun = sun(scene.sky.time, scene.sky.tilt);
            let eye = view.inverse().w_axis;
            if register == 1 {
                // The two colors go in squared, and come back out under a root: what the shader
                // works out is a light, and it is gathered in the square of the color rather than
                // in the color. The sky ramp and the shadow's own two numbers are the same in
                // every frame measured.
                let squared = |held: Vec3| held * held;
                let (diffuse, ambient) = (squared(held.diffuse), squared(held.ambient));
                write(&mut out, 0, &[sun.x, sun.y, sun.z, CLOUD_FLOOR]);
                write(&mut out, 1, &[diffuse.x, diffuse.y, diffuse.z, 1.0]);
                write(&mut out, 2, &[ambient.x, ambient.y, ambient.z, 0.0]);
                write(&mut out, 3, &[2.0, 0.0, 10.0, -5.0]);
                write(&mut out, 4, &[0.125, 50.0, 1.0, 0.0]);
                return out;
            }
            let up = sun.y.abs();
            match pass {
                // A cylinder the vertex shader flares into a cone: the first pair leans it toward
                // the sun, and the two heights it splits the reach into are how far up the band
                // stands on the near side and on the far one.
                Pass::CloudBand => {
                    let lean = glam::Vec2::new(sun.x, sun.z.abs()).normalize_or_zero() * 0.5;
                    write(&mut out, 0, &[1.0, 1.0, 0.0, 0.0]);
                    write(
                        &mut out,
                        1,
                        &[
                            lean.x,
                            lean.y,
                            held.reach * (0.25 + 0.75 * up),
                            held.reach * 0.75 * (1.0 - up),
                        ],
                    );
                    write(&mut out, 2, &[0.0; 4]);
                    write(&mut out, 3, &[sun.x, sun.y, sun.z, CLOUD_FLOOR]);
                    write(&mut out, 4, &[eye.x, eye.y, eye.z, eye.y / 1000.0]);
                }
                // The sheet tiles its texture ten times across the forty thousand units it spans,
                // and takes the whole of its first layer: the second is what the crossfading
                // variant reads, and every frame measured leaves it no weight at all.
                _ => {
                    write(&mut out, 0, &[SHEET_TILING, SHEET_TILING, 0.0, 0.0]);
                    write(&mut out, 1, &[1.0; 4]);
                    write(&mut out, 2, &[0.0; 4]);
                    write(&mut out, 3, &[sun.x, sun.y, sun.z, 0.0]);
                    write(&mut out, 4, &[eye.x, eye.y, eye.z, 0.0]);
                }
            }
            return out;
        }
        if self.name == FOG_PARAM {
            let held = scene.fog;
            // Divided by the exposure the frame is read under, the way the sky it fades toward
            // already is: the two are mixed together and have to stand in one space.
            let color = held.color / adapted;
            let glow = held.glow / adapted;
            let eye = view.inverse().w_axis;
            // What the depth buffer holds and the distance in front of the camera it stands for are
            // one over the other about the planes the projection states. The table is addressed
            // across what the fog spans, on its own texel centers: the first holds where the fog
            // starts and the last where it stops changing.
            let (z, w) = (projection.z_axis.z, projection.w_axis.z);
            let texel = 1.0 / FOG_TABLE as f32;
            let scale = (1.0 - texel) / (held.far() - held.start).max(f32::EPSILON);
            write(&mut out, 0, &[color.x, color.y, color.z, held.cap / adapted]);
            // The color carries its own weight here and the set's again in the height buffer. The
            // two only ever multiply, so the file's is folded into the color and this stays one.
            write(&mut out, 1, &[glow.x, glow.y, glow.z, 1.0]);
            // A one in the last lane is what keeps the pass off the froxel volume it would
            // otherwise march, and nothing here builds one.
            let sun = sun(scene.sky.time, scene.sky.tilt);
            write(&mut out, 2, &[sun.x, sun.y, sun.z, 1.0]);
            write(&mut out, 3, &[0.0, 0.0, 0.0, texel * 0.5 - scale * held.start]);
            write(&mut out, 4, &[eye.x, eye.y, eye.z, scale]);
            write(&mut out, 5, &[z / w, 1.0 / w, 0.0, 0.0]);
            return out;
        }
        // What takes a pixel back out to where it stands. The pass hands it the depth as sampled,
        // which the translator's own fixup leaves in the clip space the game's shaders were built
        // for, so the projection goes in as it is.
        if self.name == CLIP_TO_WORLD {
            write_rows(&mut out, &rows((projection * view).inverse(), 4));
            return out;
        }
        // Where a pixel stands in the sun's own map. The pass hands it a view-space position, and
        // rows nought and one answer the coordinate while row two answers the depth to compare, so
        // only those two take the half that turns a clip coordinate into a texture one.
        if self.name == DIRECTIONAL_SHADOW_PARAM {
            let (sun, onto) = shadow_camera(scene.light, view);
            let half = Mat4::from_cols(
                Vec4::new(0.5, 0.0, 0.0, 0.0),
                Vec4::new(0.0, 0.5, 0.0, 0.0),
                Vec4::new(0.0, 0.0, 1.0, 0.0),
                Vec4::new(0.5, 0.5, 0.0, 1.0),
            );
            put(
                DIRECTIONAL_SHADOW_PARAM,
                "m_ShadowProjectionMatrix",
                rows(half * onto * sun * view.inverse(), 4),
            );
            // Read by the lighting rather than by the resolve, and a nought here is what makes the
            // shadowed variant write black whatever the mask holds.
            put(DIRECTIONAL_SHADOW_PARAM, "m_ShadowMapParameter", vec![
                1.0 / SHADOW_MAP as f32,
                1.0 / SHADOW_MAP as f32,
                0.0,
                1.0,
            ]);
            return out;
        }
        if self.name == SHADOW_BIAS_PARAM {
            // Measured off a frame the game drew: no constant offset, a twentieth of a unit along
            // the normal, and the whole of the slope term.
            write(&mut out, 0, &[0.0, 0.05, 1.0, 0.0]);
            return out;
        }
        if self.name == HEIGHT_FOG_PARAM {
            let held = scene.fog;
            let [near, far] = held.layers;
            write(&mut out, 0, &[held.near, near.x, near.y, near.z]);
            write(&mut out, 1, &[held.clear, far.x, far.y, far.z]);
            write(
                &mut out,
                2,
                &[
                    held.haze,
                    held.glow_strength,
                    held.glow_sharpness,
                    held.glow_start,
                ],
            );
            return out;
        }
        if self.name == COMMON_TEX_PARAM {
            write(&mut out, 0, &[1.0 / adapted, adapted, 0.0, 0.0]);
            return out;
        }
        if self.name == TONE_MAP_PARAM {
            // Half a texel of the curve, which is both where its first texel's center falls and the
            // share of the exposure one texel of it spans.
            let half = 0.5 / CURVE as f32;
            write(
                &mut out,
                0,
                &[exposure.strength, exposure.shoulder, 1.0 / adapted, 0.0],
            );
            write(&mut out, 1, &[half, 1.0 - half, adapted, adapted * half]);
            return out;
        }
        if self.name == FXAA_PARAM {
            let look = scene.look;
            write(
                &mut out,
                0,
                &[1.0 / size.0, 1.0 / size.1, 1.0 - look.subpix, look.edge],
            );
            return out;
        }
        if self.name == VIEW_DEPTH_FACTOR {
            // The reading and the distance are one over the other about the plane the projection
            // states, so the pass is given that relation rather than the planes. The normal is left
            // at the scale it arrived with: the occlusion pass has a bias of its own.
            let (z, w) = (projection.z_axis.z, projection.w_axis.z);
            write(&mut out, 0, &[z / w, 1.0 / w, 1.0, 0.0]);
            return out;
        }
        if self.name == VIEW_ROTATION {
            write(&mut out, 0, &rows(view, 3));
            return out;
        }
        if self.name == HDAO_PARAM {
            let look = scene.look;
            let (near, far) = scene.planes();
            let far = far.max(f32::EPSILON);
            let span = (far - near).max(f32::EPSILON);
            let scale = OCCLUSION_SCALE as f32;
            let reach = look.distance * far;
            write(
                &mut out,
                0,
                &[look.radius, look.radius, scale / size.0, scale / size.1],
            );
            write(
                &mut out,
                1,
                &[
                    look.accept,
                    1.0 / (look.reject * span).max(f32::EPSILON),
                    1.0 / (look.fade * far).max(f32::EPSILON),
                    reach,
                ],
            );
            write(
                &mut out,
                2,
                &[reach, look.bias * span, look.intensity, look.power],
            );
            return out;
        }
        let world_view = view * model;
        let view_projection = projection * view;
        // Nothing here moves between frames, so every previous-frame matrix is the current one and
        // the motion vectors come out as nought.
        let camera = "g_CameraParameter";
        for name in ["m_ViewMatrix", "m_ViewMatrixPrev"] {
            put(camera, name, rows(view, 3));
        }
        for name in [
            "m_InverseViewMatrix",
            "m_InverseViewMatrixPrev",
            "m_MainViewToWorldMatrix",
        ] {
            put(camera, name, rows(view.inverse(), 3));
        }
        for name in ["m_ViewProjectionMatrix", "m_ViewProjectionMatrixPrev"] {
            put(camera, name, rows(view_projection, 4));
        }
        for name in [
            "m_InverseViewProjectionMatrix",
            "m_InverseViewProjectionMatrixPrev",
        ] {
            put(camera, name, rows(view_projection.inverse(), 4));
        }
        for name in [
            "m_ProjectionMatrix",
            "m_ProjectionMatrixPrev",
            "m_MainViewToProjectionMatrix",
        ] {
            put(camera, name, rows(projection, 4));
        }
        for name in ["m_InverseProjectionMatrix", "m_InverseProjectionMatrixPrev"] {
            put(camera, name, rows(projection.inverse(), 4));
        }
        put(camera, "m_ProjToProjPrevMatrix", rows(Mat4::IDENTITY, 4));
        put(camera, "m_ViewToViewPrevMatrix", rows(Mat4::IDENTITY, 3));
        // The transform a vertex shader multiplies by before the projection alone, with nothing
        // between the two: it takes an object into view space rather than into the world. The buffer
        // holds this frame's and the last one's.
        put("g_WorldViewMatrix", "g_WorldViewMatrix", {
            let mut held = rows(world_view, 3);
            held.extend(rows(world_view, 3));
            held
        });
        // What takes an object into the world alone, which the engine's own draws carry instead of
        // the object transform a model's instancing buffer holds.
        put("g_WorldMatrix", "g_WorldMatrix", rows(model, 3));
        put(INSTANCE, "m_MulColor", vec![1.0; 4]);
        // Declared by the five character packages and read by none of them: nothing in `character`,
        // `characterlegacy`, `hair`, `iris` or `skin` touches it once. Left at the identity because
        // a package outside those five may yet read it, and one costs nothing where nought would be
        // the lane that switches a thing off.
        put(INSTANCE, "m_Param", vec![1.0]);
        // One record an eye, picked by the vertex color. The first two lanes scale the coordinate an
        // eye's textures are read at and the third warps it toward the pupil, so ones leave that
        // coordinate where the mesh's own uv put it; nought collapses the eye onto a single texel.
        put(INSTANCE, "m_IrisParam", vec![1.0; 8]);
        // The engine drives this per draw and no material states one, so identity is what leaves a
        // table's own emissive column as it was written.
        put(
            "g_MaterialParameterDynamic",
            "m_EmissiveColor",
            vec![1.0; 3],
        );
        put("g_ModelParameter", "m_Params", vec![1.0; 4]);
        // What skin showing through a stocking is multiplied by, which is not the light's own color
        // of the same name.
        put("g_SkinMaterialParameter", "m_DiffuseColor", vec![1.0; 3]);

        // The colors a character was made with. The last lane of the two hair colors is where a
        // decal is read from.
        let held = scene.customize;
        let customize = "g_CustomizeParameter";
        put(customize, "m_SkinColor", held.skin.to_vec());
        put(customize, "m_LipColor", held.lip.to_vec());
        put(customize, "m_MainColor", held.hair.to_vec());
        put(customize, "m_MeshColor", held.highlight.to_vec());
        put(customize, "m_LeftColor", held.left_eye.to_vec());
        put(customize, "m_RightColor", held.right_eye.to_vec());
        put(customize, "m_OptionColor0", held.option.to_vec());

        // A pixel's own place, which a screen-wide pass has nothing else to work from. The row a
        // texture coordinate names counts from the far side of the one a fragment coordinate does,
        // so the height goes in negative and the offset takes it back.
        let (width, height) = (size.0.max(1.0), size.1.max(1.0));
        let common = "g_CommonParameter";
        put(
            common,
            "m_RenderTarget",
            vec![1.0 / width, -1.0 / height, 0.0, 1.0],
        );
        put(
            common,
            "m_Viewport",
            vec![2.0 / width, -2.0 / height, -1.0, 1.0],
        );
        put(common, "m_Misc", vec![1.0, 1.0, 0.0, 0.0]);
        put(common, "m_Misc2", vec![1.0, 0.0, 0.0, 0.0]);
        let screen = "g_ScreenParameter";
        put(screen, "m_BackBufferSize", vec![width, height]);
        put(screen, "m_ViewportSize", vec![width, height]);
        for name in ["m_InverseBackBufferSize", "m_InverseViewportSize"] {
            put(screen, name, vec![1.0 / width, 1.0 / height]);
        }
        // Nothing here renders at a resolution other than the one it presents at, and a pass that
        // reads the frame back scales its coordinate by this before sampling.
        for name in ["m_DynamicResolutionScale", "m_DynamicResolutionChangeScale"] {
            put(screen, name, vec![1.0; 2]);
        }

        // The transform water is drawn through: it takes one from here rather than from the buffer
        // every other package names, and at nought every vertex lands on the same point.
        put(INSTANCE, "m_WorldViewMatrix", rows(world_view, 3));
        // The fade a dither clip tests against, and the weight a mesh's own position carries into
        // the wave it is lifted by. One leaves both as the file wrote them.
        put(INSTANCE, "m_Misc", vec![0.0, 0.0, 1.0, 1.0]);

        // Water is a sum of Gerstner waves, and each is a sine of a frequency times this plus a
        // wavenumber times where the vertex stands; the wave maps, the noise and the caustics all
        // scroll along it as well, at rates the material states.
        let water = "g_WaterParameter";
        put(water, "m_WavingParam", vec![clock; 4]);
        for name in ["m_GBufferSize", "m_RenderTargetSize"] {
            put(water, name, vec![width, height, 1.0 / width, 1.0 / height]);
        }
        for name in ["m_GBufferPixelSize", "m_RenderTargetPixelSize"] {
            put(water, name, vec![1.0 / width, 1.0 / height, width, height]);
        }
        put(
            water,
            "m_HalfViewPositionPixelSize",
            vec![2.0 / width, 2.0 / height, width * 0.5, height * 0.5],
        );
        // How far into the frame a surface may reach for what stands behind it. Sampling past this
        // is folded back in, so the whole frame is what leaves the reading where it was aimed.
        put(water, "m_DynamicViewportResolution", vec![1.0; 4]);
        // Both lerp toward one against a weight the mesh carries, so a one is the reading the
        // engine's own number would only move away from.
        put(water, "m_Roughness", vec![1.0; 4]);
        put(water, "m_Misc", vec![1.0; 4]);
        put(water, "m_NoiseSize", vec![1.0; 4]);

        // The wind carries the whole reach: a mesh weights it down to a tenth at most, which is what
        // leaves the stated strength in world units. The pair below is read by every one of the
        // twenty-eight shaders holding the buffer, and the two past it by none of them.
        let waving = "g_WavingParam";
        let wind = scene.wind.heading * scene.wind.reach;
        put(waving, "m_WindVector", wind.to_array().to_vec());
        put(waving, "m_UpVector", vec![0.0, 1.0, 0.0]);
        put(waving, "m_WavingParam", vec![1.0, 1.0, 0.0, 0.0]);

        // A light is read in view space: the shader dots its direction against a normal it has just
        // brought out of the G-buffer and through the view matrix.
        let axes = glam::Mat3::from_mat4(view);
        let light = "g_LightParam";
        let lamp = scene.lamp;
        put(
            light,
            "m_Direction",
            (axes
                * match pass {
                    Pass::Lamp => lamp.direction,
                    _ => scene.light,
                })
            .normalize_or_zero()
            .to_array()
            .to_vec(),
        );
        let color = match pass {
            Pass::Lamp => lamp.color,
            _ => scene.diffuse,
        };
        put(light, "m_DiffuseColor", color.to_array().to_vec());
        put(
            light,
            "m_SpecularColor",
            match pass {
                Pass::Lamp => lamp.color,
                _ => scene.specular,
            }
            .to_array()
            .to_vec(),
        );
        // The two lighting packages read this buffer differently. A sun fades with the depth of the
        // pixel, and the fade is off: the scale is cubed and clamped, so a constant one leaves it
        // alone, and a frame the game drew states the same `(0, 0, 1, 0.05)` - the floor never bites
        // against a ramp already at one. A lamp reads `z` as what its squared distance is taken into
        // the ramp by, which is its clip volume, and `w` as how far it stays at full strength before
        // the inverse-distance term stops saturating. Only a spot's own shader reads `y`, and the
        // sun's reads the lane whatever is in it.
        let reach = lamp.reach();
        let cone = match pass {
            Pass::Lamp => lamp.cone,
            _ => 0.0,
        };
        put(
            light,
            "m_Attenuation",
            match pass {
                Pass::Composite | Pass::CompositeBlended => vec![0.0, 0.0, 1.0, 0.05],
                _ => vec![0.0, cone, 1.0 / (reach * reach), lamp.range.max(0.001)],
            },
        );
        put(light, "m_LightFadeValueStatic", vec![1.0]);
        put(light, "m_LightFadeValueDynamic", vec![1.0]);

        // A lamp is drawn as the volume it reaches: its own vertex shader clamps a unit box to the
        // extents the zone clips it against and then projects, so the transform carries the light's
        // whole reach and the extents cut the box back out of it. A spot scales the box by the
        // fourth extent before clamping and keeps only the half in front of itself, so one leaves it
        // where the clamp alone would have put it.
        let volume = lamp.placement * Mat4::from_scale(Vec3::splat(reach));
        let (min, max) = lamp.clip();
        put(
            light,
            "m_Position",
            (view * lamp.placement * Vec3::ZERO.extend(1.0))
                .to_array()
                .to_vec(),
        );
        put(light, "m_ClipMin", min.extend(1.0).to_array().to_vec());
        put(light, "m_ClipMax", max.to_array().to_vec());
        put(
            light,
            "m_WorldViewProjectionMatrix",
            rows(view_projection * volume, 4),
        );
        put(
            light,
            "m_WorldViewInversMatrix",
            rows((view * volume).inverse(), 3),
        );
        out
    }

    /// One record per object drawn, at the stride the reflection's own record takes. The transform
    /// takes an object into view space rather than into the world: what a shader multiplies by after
    /// it is the projection alone.
    fn instancing(&self, scene: &Scene, instances: &[Instance], out: &mut [u8]) {
        let stride = self.stride() as usize;
        let mut put = |at: usize, name: &str, values: &[f32]| {
            let Some(member) = self.members.iter().find(|held| held.name == name) else {
                return;
            };
            for (lane, value) in values.iter().enumerate() {
                let offset = at * stride + member.offset as usize + lane * 4;
                if offset + 4 <= out.len() {
                    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
        };
        let held = [Instance {
            transform: scene.model,
            ..Instance::default()
        }];
        let instances = match instances.is_empty() {
            true => &held[..],
            false => instances,
        };
        for (at, instance) in instances.iter().enumerate().take(self.instances()) {
            let world_view = scene.view * instance.transform;
            put(
                at,
                "m_TransformMatrix",
                &world_view.transpose().to_cols_array()[..12],
            );
            put(at, "m_SkyVisibility", &[instance.sky_visibility]);
            put(at, "m_DitherAlpha", &[1.0]);
            // The phase one object sways at. Its own place sets where in the cycle it starts, so a
            // stand of the same plant does not lean as one; the noise is the same offset again,
            // which is all the vertical bob reads it for.
            let (x, z) = (instance.transform.w_axis.x, instance.transform.w_axis.z);
            let offset = (x * 0.37 + z * 0.61).rem_euclid(std::f32::consts::TAU);
            put(at, "m_WavingAnimTime", &[scene.clock * scene.wind.rate + offset]);
            put(at, "m_WavingAnimNoize", &[(offset / std::f32::consts::TAU).fract()]);
            // At the strength that leaves a surface emitting what its own material states. Left at
            // nought the shading takes its non-emissive branch, and every glowing thing a zone
            // places - a crystal naming an emissive colour of 2.89 among them - comes out dark. No
            // file found states a per-object strength, so this is the identity rather than a value.
            put(at, "m_EmissivePower", &[1.0]);
            put(at, "m_EmissiveColor", &[1.0, 1.0, 1.0]);
        }
    }
}

/// One register of a buffer written as floats.
fn write(out: &mut [u8], register: usize, values: &[f32]) {
    for (at, value) in values.iter().enumerate() {
        let offset = register * 16 + at * 4;
        if offset + 4 <= out.len() {
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
}

/// The ambient array, whose header the reflection describes and whose entries it does not: past the
/// spherical harmonics the buffer is an array of a struct named but not laid out, so it goes in by
/// register.
///
/// One entry is filled and the count says one, which is what keeps the composite from walking the
/// array at all: it takes entry `count - 1` at full weight and never enters the loop. The composite
/// reads entry `n` at registers `12 * n + 4` through `12 * n + 15`, so the one entry starts at four.
///
/// The harmonics go in turned by `axes`. The composite dots the light rows against a normal it has
/// just taken through the view matrix and the sky rows against a reflection of that same normal, so
/// both are read in view space; the reflection only goes back to the world to sample the cube.
fn ambient(held: &Ambient, axes: glam::Mat3, view: Mat4, out: &mut [u8]) {
    if out.len() < 8 {
        return;
    }
    let turned = |row: &Vec4| (axes * row.truncate()).extend(row.w);
    // The count reads as a whole number rather than as the float that would print the same.
    let volumes = held.volumes.len().min(ENTRIES - 1);
    out[..4].copy_from_slice(&(1 + volumes as u32).to_le_bytes());
    out[4..8].copy_from_slice(&held.sky_scale.to_le_bytes());
    for (at, row) in held.sky.iter().enumerate() {
        write(out, 1 + at, &turned(row).to_array());
    }
    entry(held, axes, out, 4);
    // No bounding shape, so the entry covers the frame rather than a room.
    write(out, 14, &[0.0, 0.0, 0.0, 0.0]);
    write(out, 15, &[0.0, 1.0, 0.0, 0.0]);

    // The places that light themselves, each tested against the pixel before the one above it is
    // fallen back on. The composite reads entry `n` from `12n + 4`.
    for (index, volume) in held.volumes.iter().take(volumes).enumerate() {
        let at = (index + 1) * 12 + 4;
        for (row, held) in volume.light.iter().enumerate() {
            write(out, at + row, &turned(held).to_array());
        }
        write(out, at + 3, &[0.0, 0.0, 0.0, volume.scale]);
        // A bounded entry states no attenuation and takes its reflected term raw, which is the pair
        // the composite tests to decide the place lights itself.
        write(out, at + 4, &[held.fade.x, held.fade.y, held.fade.z, 0.0]);
        write(out, at + 5, &[1.0, 0.0, 1.0, held.capture]);
        // Into the volume's own space, where it stands as the unit shape. The buffer holds the
        // three rows a `float3x4` takes, and the pixel arrives in front of the camera, so the view
        // is folded in here rather than at the source.
        // A pixel arrives in front of the camera, so the view is undone before the volume's own
        // transform takes it in.
        let into = volume.into * view.inverse();
        let rows = into.transpose().to_cols_array();
        for row in 0..3 {
            write(out, at + 6 + row, &rows[row * 4..row * 4 + 4]);
        }
        // How sharply it takes over across each face. The composite reads the near widths outright
        // and works the far ones back out by subtracting them, so both sides carry the same number.
        write(
            out,
            at + 9,
            &[volume.fade.x, volume.fade.y, volume.fade.z, volume.fade.x],
        );
        write(
            out,
            at + 10,
            &[volume.fade.y, volume.fade.z, 0.0, volume.shape],
        );
        write(out, at + 11, &[0.0, 1.0, 0.0, 0.0]);
    }
}

/// The ten registers a composite reads one entry of the ambient from. A drawing package that
/// composites itself binds exactly these as `g_AmbientParam`, which is what says where each field
/// sits. An array entry shares the first six and then diverges: what the sky rows sit at here is a
/// bounding volume there, tested against the pixel's position rather than dotted against a
/// direction, and left unread while the shape register stays nought.
fn entry(held: &Ambient, axes: glam::Mat3, out: &mut [u8], at: usize) {
    let turned = |row: &Vec4| (axes * row.truncate()).extend(row.w);
    for (row, held) in held.light.iter().enumerate() {
        write(out, at + row, &turned(held).to_array());
    }
    write(out, at + 3, &[0.0, 0.0, 0.0, held.scale]);
    write(out, at + 4, &[held.fade.x, held.fade.y, held.fade.z, 1.0]);
    write(
        out,
        at + 5,
        &[
            held.reflection.x,
            held.reflection.y,
            held.reflection.z,
            held.capture,
        ],
    );
    for (row, held) in held.sky.iter().enumerate() {
        write(out, at + 6 + row, &turned(held).to_array());
    }
    write(out, at + 9, &[held.sky_scale, 0.0, 0.0, 0.0]);
}

/// The joint transforms a skinned shader reads, as the dwords of the texture standing in for a
/// structured buffer. Each is four columns of three floats.
///
/// A joint's transform is the object's own, composed with what the pose moved that bone by, so a
/// palette of identities stands the model in the pose it is stored in.
pub fn joints(palette: &[Mat4], object: Mat4) -> Vec<u32> {
    let rows = (palette.len().max(1) * JOINT).div_ceil(ROW);
    let mut out = vec![0u32; rows * ROW];
    for (at, joint) in palette.iter().enumerate() {
        let columns = (object * *joint).to_cols_array();
        for column in 0..4 {
            for lane in 0..3 {
                out[at * JOINT + column * 3 + lane] = columns[column * 4 + lane].to_bits();
            }
        }
    }
    out
}

/// The parameter files the table is filled from, and the record each one's first profile lands at: a
/// G pass adds its own family's base to the type its material names.
pub const PARAMETERS: [(usize, &str); 2] = [
    (CHARA_TYPES, "common/graphics/chara_shader_param.spm"),
    (128, "common/graphics/bg_shader_param.spm"),
];

/// The table `SV_Target.w` indexes, as the dwords of the texture standing in for a structured
/// buffer, filled from the parameter files whose profiles it holds.
///
/// Every index a G pass can write has a record, since the one it writes is `(32 + type) / 255` and
/// what a material makes of that is the material's own business. A file the caller has yet to
/// receive leaves its own records at nought, which is the branch a plain surface takes.
pub fn shader_types(files: &[(usize, &spm::ShaderParameters)]) -> Vec<u32> {
    let rows = (SHADER_TYPES * SHADER_TYPE).div_ceil(ROW);
    let mut out = vec![0u32; rows * ROW];
    for (base, file) in files {
        for profile in 0..file.rows().len() {
            let at = (base + profile) * SHADER_TYPE;
            let Some(record) = out.get_mut(at..at + SHADER_TYPE) else {
                continue;
            };
            for (column, held) in file.columns().iter().enumerate() {
                let Some(name) = spm::name(held.id()) else {
                    continue;
                };
                let Some(value) = file.value(profile, column) else {
                    continue;
                };
                if let Some((slot, stated)) = parameter(name, value) {
                    record[slot] = stated;
                }
            }
        }
    }
    out
}

/// Whether any record this material can reach states a fur length. The fur pass discards every pixel
/// whose own record leaves it at nought, so a model reaching none of them has nothing for it to do.
///
/// A material's records are the ones its colour table names a row at a time, plus the one a material
/// carrying no table states outright; both are offsets into the character family's own profiles.
/// Only that family: a background material states its profile through the same constant, and the
/// alpha the pass would march along is that family's emissive flag instead.
pub fn furred(material: &Material, types: &[u32]) -> bool {
    if material.family() == Family::Background {
        return false;
    }
    let held = material.held();
    let table = held.color_table().into_iter().flat_map(|table| {
        (0..table.rows()).filter_map(|row| Some(table.row_values(row)?.shader_index as usize))
    });
    let stated = held
        .constants()
        .iter()
        .find(|constant| constant.id() == SHADER_ID)
        .and_then(|constant| held.constant_values(constant))
        .and_then(|values| values.first().copied())
        .map(|value| value as usize);
    table.chain(stated).any(|profile| {
        types
            .get((CHARA_TYPES + profile) * SHADER_TYPE + FUR_LENGTH)
            .is_some_and(|held| f32::from_bits(*held) > 0.0)
    })
}

/// Where one of the parameters a file names goes in a record, and the dword it goes there as. A file
/// orders its own columns and carries whichever subset of the parameters its family reads; the
/// record's layout is the shaders' own, and every file writes into the same one.
fn parameter(name: &str, value: spm::Value) -> Option<(usize, u32)> {
    let slot = match name {
        "LightingType" => 0,
        "SubSurfaceProfileID" => 1,
        "SubSurfaceWidth" => 2,
        "BackScatterPower" => 3,
        "SheenRate" => 4,
        "SheenTintRate" => 5,
        "SheenAperture" => 6,
        "UseSubSurfaceRate" => 7,
        "HairScatterColorShift" => 8,
        "HairSpecularPrimaryShift" => 9,
        "HairSpecularBackScatterShift" => 10,
        "HairSpecularSecondaryShift" => 11,
        "FurLength" => 12,
        "HairBackScatterRoughnessOffsetRate" => 13,
        "HairSecondaryRoughnessOffsetRate" => 14,
        "SubSurfacePower" => 15,
        _ => return None,
    };
    let held = match value {
        // A specular shift is a lobe center against the sine of an angle, and the files state it in
        // degrees.
        spm::Value::Float(held) if name.starts_with("HairSpecular") => held.to_radians().to_bits(),
        spm::Value::Float(held) => held.to_bits(),
        spm::Value::Unsigned(held) => held,
        spm::Value::Name(held) => lighting(held),
    };
    Some((slot, held))
}

/// The lighting model a record names, as the integer the shaders compare against. Anything else is
/// the default, which is the model a surface with nothing said about it takes.
fn lighting(id: u32) -> u32 {
    match spm::name(id) {
        Some("HAIR") => 1,
        Some("LEGACY") => 2,
        Some("HALF") => 3,
        _ => 0,
    }
}

/// Halfs in a row of the layout every shader addresses, and the rows it addresses. A row address is
/// scaled by a hardcoded `1/32` everywhere, and columns nought through seven are divided by the
/// width the shader queries, so nothing else is readable.
const EXTENDED_ROW: usize = 32;
const EXTENDED_ROWS: usize = 32;

/// Where a legacy row's halfs sit in an extended one. Diffuse, specular and emissive land where they
/// were; the two scalars beside them swap; and the tile index and transform move to the end.
const LEGACY_TO_EXTENDED: [(usize, usize); 16] = [
    (0, 0),
    (1, 1),
    (2, 2),
    (7, 3),
    (4, 4),
    (5, 5),
    (6, 6),
    (3, 7),
    (8, 8),
    (9, 9),
    (10, 10),
    (11, 25),
    (12, 28),
    (13, 29),
    (14, 30),
    (15, 31),
];

/// The color table in the layout the game's own shaders read: eight texels a row, thirty-two rows.
/// Answers the halfs, the texels a row takes, and the rows.
///
/// An extended table is already that. A legacy one states sixteen rows of four texels, so it is
/// widened and each row becomes the pair the shaders address it as, which leaves the row blend a
/// no-op: legacy tables have no second row to blend toward.
pub fn table(held: &mtrl::ColorTable) -> Option<(Vec<u16>, usize, usize)> {
    let rows = held.rows();
    let raw = held.raw();
    if rows == 0 || !raw.len().is_multiple_of(rows * 4) {
        return None;
    }
    if held.kind() != mtrl::ColorTableKind::Legacy {
        return Some((raw.to_vec(), raw.len() / rows / 4, rows));
    }
    let mut values = vec![0u16; EXTENDED_ROWS * EXTENDED_ROW];
    for pair in 0..rows.min(EXTENDED_ROWS / 2) {
        let Some(row) = held.row(pair) else { continue };
        for (from, to) in LEGACY_TO_EXTENDED {
            let Some(half) = row.get(from) else { continue };
            values[pair * 2 * EXTENDED_ROW + to] = *half;
            values[(pair * 2 + 1) * EXTENDED_ROW + to] = *half;
        }
    }
    Some((values, EXTENDED_ROW / 4, EXTENDED_ROWS))
}

#[cfg(test)]
mod test {
    use std::io::Cursor;

    use glam::{Mat3, Mat4, Vec3, Vec4};
    use ironworks::file::{File, spm::ShaderParameters};

    use super::{
        Ambient, Buffer, Exposure, FOG_PARAM, Fog, JOINT, ROW, SHADER_TYPE, SUN_PARAM, Pass, Scene,
        Sky, Volume, ambient, joints, selector, shader_types, sun,
    };

    /// The three buffers the exposure chain reads, against the bytes a capture of the running game
    /// held in them. What the environment stated at that time and weather goes in; what the frame
    /// was measured and read under has to come out.
    #[test]
    fn the_exposure_buffers_come_out_as_the_game_held_them() {
        let scene = Scene {
            exposure: Exposure {
                min: 1.0,
                max: 3.525834,
                rate: 2.0,
                key: 0.347417,
                strength: 0.5,
                shoulder: 0.95,
                step: 0.027_392_5,
                adapted: 1.431022,
            },
            ..Default::default()
        };
        let filled = |name: &str, registers| {
            let held = Buffer {
                name: name.to_owned(),
                members: Vec::new(),
                registers,
                fixed: None,
            };
            held.fill(&scene, Pass::Composite, &[])
                .chunks_exact(4)
                .map(|held| f32::from_le_bytes(held.try_into().unwrap()))
                .collect::<Vec<f32>>()
        };
        let close = |held: &[f32], want: &[f32]| {
            held.iter()
                .zip(want)
                .all(|(held, want)| (held - want).abs() <= want.abs() * 1e-4 + 1e-6)
        };

        // The key goes in squared and the rate scaled by the frame, which is the whole of what the
        // capture proved and neither of which is guessable off the field names.
        assert!(close(
            &filled("cAdaptLumParam", 1),
            &[1.0, 3.525834, 0.054785, 0.120698]
        ));
        assert!(close(&filled("cCommonTexParam", 1), &[0.698801, 1.431022, 0.0, 0.0]));
        // The curve's bounds are half a texel in from either end of a strip 1024 wide, and its last
        // lane is the exposure over twice that. The game held `z` a frame older than the rest, so
        // 1.432421 there rather than the 1.431022 this fills.
        assert!(close(
            &filled("cToneMapParam", 2),
            &[
                0.5,
                0.95,
                0.698801,
                0.0,
                0.00048828125,
                0.99951171875,
                1.431022,
                0.000698741,
            ]
        ));
    }

    /// The fog reads a distance out of the depth buffer as `1 / (y * d + x)`, and everything it then
    /// does with that distance rests on those two lanes. Rather than argue the convention, this
    /// pushes a distance through the projection the zone is drawn with and asks for it back.
    #[test]
    fn a_depth_reading_comes_back_the_distance_it_stood_for() {
        let projection = Mat4::perspective_rh(1.0, 1.6, 0.1, 8000.0);
        let scene = Scene {
            projection,
            fog: Fog {
                cap: 0.9,
                rate: 0.0005,
                start: 100.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let held = Buffer {
            name: FOG_PARAM.to_owned(),
            members: Vec::new(),
            registers: 6,
            fixed: None,
        };
        let filled: Vec<f32> = held
            .fill(&scene, Pass::Composite, &[])
            .chunks_exact(4)
            .map(|held| f32::from_le_bytes(held.try_into().unwrap()))
            .collect();
        for want in [1.0f32, 100.0, 500.0, 2000.0] {
            // Where the projection leaves a point that far in front of the camera, and what the
            // shaders' own fixup makes of it: they hand the card a clip depth over the whole of
            // `[-w, w]`, and the buffer holds the half of that between nought and one.
            let clip = projection * glam::Vec4::new(0.0, 0.0, -want, 1.0);
            let depth = (clip.z / clip.w * 2.0 - 1.0) * 0.5 + 0.5;
            let read = 1.0 / (filled[21] * depth + filled[20]);
            assert!((read - want).abs() < want * 1e-3, "{want} came back {read}");
        }
        // Texel nought stands where the fog starts, and the last where its opacity reaches the cap.
        let coordinate = |z: f32| filled[19] * z + filled[15];
        assert!((coordinate(100.0) - 0.5 / 256.0).abs() < 1e-6);
        assert!((coordinate(1900.0) - 255.5 / 256.0).abs() < 1e-6);
    }

    /// A camera turned to face the sun should find it dead centre. The pass measures every pixel
    /// against the place this states, so an error here moves the whole glow rather than distorting
    /// it, which on screen reads as a sun in the wrong part of the sky.
    #[test]
    fn the_sun_lands_where_the_camera_looks() {
        let time = 51_000.0;
        let tilt = 5.0;
        let toward = sun(time, tilt);
        let eye = Vec3::new(-6.535, 18.583, 36.727);
        let projection = Mat4::perspective_rh(55.0f32.to_radians(), 1251.0 / 913.0, 0.1, 8000.0);
        let scene = Scene {
            view: Mat4::look_at_rh(eye, eye + toward, Vec3::Y),
            projection,
            size: (1251.0, 913.0),
            sky: Sky {
                time,
                tilt,
                ..Default::default()
            },
            ..Default::default()
        };
        let held = Buffer {
            name: SUN_PARAM.to_owned(),
            members: Vec::new(),
            registers: 5,
            fixed: None,
        };
        let filled: Vec<f32> = held
            .fill(&scene, Pass::Composite, &[])
            .chunks_exact(4)
            .map(|held| f32::from_le_bytes(held.try_into().unwrap()))
            .collect();
        assert!(
            (filled[2] - 0.5).abs() < 1e-4 && (filled[3] - 0.5).abs() < 1e-4,
            "the sun stands at {}, {} rather than the middle",
            filled[2],
            filled[3]
        );
    }

    /// The composite reads entry `n` at registers `12 * n + 4` through `12 * n + 15`, and its header
    /// at nought through three. Nothing in the reflection lays the entry out, so this is the whole
    /// statement of where each field goes.
    #[test]
    fn the_ambient_entry_starts_at_the_fourth_register() {
        let held = Ambient {
            sky: [Vec4::splat(1.0), Vec4::splat(2.0), Vec4::splat(3.0)],
            sky_scale: 4.0,
            light: [Vec4::splat(5.0), Vec4::splat(6.0), Vec4::splat(7.0)],
            scale: 8.0,
            fade: Vec3::new(9.0, 10.0, 11.0),
            reflection: Vec3::new(12.0, 13.0, 14.0),
            capture: 15.0,
            haze: Vec4::ZERO,
            volumes: std::sync::Arc::from([] as [Volume; 0]),
        };
        let mut out = vec![0u8; 16 * 16];
        ambient(&held, Mat3::IDENTITY, Mat4::IDENTITY, &mut out);
        let lane = |register: usize, at: usize| {
            let start = register * 16 + at * 4;
            f32::from_le_bytes(out[start..start + 4].try_into().unwrap())
        };
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 1);
        assert_eq!(lane(0, 1), 4.0);
        assert_eq!([lane(1, 0), lane(2, 0), lane(3, 0)], [1.0, 2.0, 3.0]);
        assert_eq!([lane(4, 0), lane(5, 0), lane(6, 0)], [5.0, 6.0, 7.0]);
        assert_eq!(lane(7, 3), 8.0);
        assert_eq!([lane(8, 0), lane(8, 1), lane(8, 2)], [9.0, 10.0, 11.0]);
        assert_eq!(
            [lane(9, 0), lane(9, 1), lane(9, 2), lane(9, 3)],
            [12.0, 13.0, 14.0, 15.0]
        );
        // Past the entry's own twelve registers nothing is written: the next one along is entry one.
        assert!(out[16 * 16..].is_empty());
    }

    /// The row is dotted against a normal and a one, and the file runs constant, `y`, `z`, `x`. Each
    /// term carries the weight a cosine lobe gathers it at, which a real frame's own buffer matches
    /// to seven figures; the ratio between the two weights is `2/sqrt(3)`.
    #[test]
    fn a_harmonic_row_is_convolved_and_puts_the_constant_last() {
        let row = Ambient::row(&[1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let close = |held: f32, want: f32| (held - want).abs() < 1e-6;
        assert!(close(row.w, 0.886_226_9));
        assert!(close(row.x, 4.0 * 1.023_326_7));
        assert!(close(row.y, 2.0 * 1.023_326_7));
        assert!(close(row.z, 3.0 * 1.023_326_7));
        assert!(close(row.x / row.w / 4.0, 2.0 / 3.0f32.sqrt()));
        // The `y` lane is what a normal pointing up reads, beside the constant every normal reads.
        assert!(close(
            row.dot(Vec4::new(0.0, 1.0, 0.0, 1.0)),
            2.0 * 1.023_326_7 + 0.886_226_9
        ));
    }

    #[test]
    fn the_selector_is_a_polynomial_in_thirty_one() {
        assert_eq!(selector(&[]), 0);
        assert_eq!(selector(&[7]), 7);
        assert_eq!(selector(&[1, 1]), 32);
        assert_eq!(selector(&[0, 0, 1]), 961);
        assert_eq!(selector(&[u32::MAX, 2]), u32::MAX.wrapping_add(62));
    }

    /// Four columns of three floats each, which is how the shader rebuilds a row: it takes the first
    /// component of each of its four reads.
    #[test]
    fn a_joint_is_four_columns_of_three() {
        let held = joints(
            &[Mat4::IDENTITY; 2],
            Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0)),
        );
        assert_eq!(held.len(), ROW);
        let value = |lane: usize| f32::from_bits(held[lane]);
        assert_eq!(
            (0..JOINT).map(value).collect::<Vec<_>>(),
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 4.0, 5.0, 6.0]
        );
        assert_eq!(&held[JOINT..JOINT * 2], &held[..JOINT]);
    }

    /// A parameter file of one profile, written the way the shipping ones are.
    fn parameters(columns: &[(u32, u32)], values: &[u32]) -> Vec<u8> {
        // Offsets are counted in words, and the header is three of them.
        let columns_at = 3u16;
        let rows_at = columns_at + 2 * columns.len() as u16;
        let values_at = rows_at + 2;

        let mut bytes = 0x0100_0000u32.to_le_bytes().to_vec();
        bytes.push(columns.len() as u8);
        bytes.push(1);
        bytes.extend(columns_at.to_le_bytes());
        bytes.extend(rows_at.to_le_bytes());
        bytes.extend(values_at.to_le_bytes());
        for (id, kind) in columns {
            bytes.extend(id.to_le_bytes());
            bytes.extend(kind.to_le_bytes());
        }
        bytes.extend(0xB9FD_FB6Cu32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        for value in values {
            bytes.extend(value.to_le_bytes());
        }
        bytes
    }

    /// Nothing in a file says where its parameters go: the record is laid out by what the shaders
    /// read, and a file states whichever of them its own family uses, in whichever order it likes.
    #[test]
    fn a_profile_fills_the_record_the_shaders_read() {
        let file = ShaderParameters::read(Cursor::new(parameters(
            &[
                (0xF33F_F064, 0),
                (0x8FB5_3404, 1),
                (0xE800_1A59, 2),
                (0x4133_8E94, 0),
            ],
            &[13.0f32.to_bits(), 5, 0x56F1_6FCB, 1.0f32.to_bits()],
        )))
        .unwrap();

        let held = shader_types(&[(32, &file)]);
        let record = &held[32 * SHADER_TYPE..33 * SHADER_TYPE];
        assert_eq!(record[0], 2);
        assert_eq!(record[1], 5);
        assert_eq!(f32::from_bits(record[3]), 1.0);
        // The lobe this centers is a Gaussian over a sine, and the file states it in degrees.
        assert_eq!(f32::from_bits(record[9]), 13.0f32.to_radians());
        assert!(held[..32 * SHADER_TYPE].iter().all(|held| *held == 0));
    }
}
