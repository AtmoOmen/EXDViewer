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
/// The one furblur marches along a strand at. Its other pass is a plain four-tap square.
const PASS_FUR: u32 = 0x5bc1_ad3f;

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
pub const COMPOSITE: &str = "shader/sm5/shpk/bg_composite.shpk";
/// Softens the surface a strand grows out of, between the G-buffer and the light it is read under.
pub const FUR: &str = "shader/sm5/shpk/furblur.shpk";

/// The members of the game's post chain the viewer runs. The first reads a table a file holds, where
/// the exposure and the tone curve before it are targets the engine builds a frame at a time off
/// constants no file states. The other two smooth the frame's edges, in the order they run: one
/// writes each pixel's brightness into the alpha the next reads its edges off.
pub const TONE_ADJUST: &str = "shader/sm5/posteffect/ToneAdjust.shcd";
pub const FXAA_LUMA: &str = "shader/sm5/posteffect/FXAALuma.shcd";
pub const FXAA: &str = "shader/sm5/posteffect/FXAA.shcd";

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

/// The buffer it reads, and the frame and the table it reads them through.
const TONE_MAP_PARAM: &str = "cToneMapParam";
pub const POST_INPUT: &str = "sInput";
pub const POST_TABLE: &str = "sLUT";

/// What the pass takes of that buffer: `w` is the exponent the frame is raised to before the table,
/// and `z` how much of the table's answer reaches the frame, which the pass skips entirely while it
/// is not positive. Neither is stated anywhere: every constant buffer of the chain reports no
/// default at all. So the exponent is left where it changes nothing, and the table is left out:
/// three of them ship, nothing states which one binds, and the exposure and tone curve a frame
/// would reach one through are not run here, so reading a table at full strength grades a frame it
/// was never authored over and takes the color out of it.
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

/// `GetDirectionalLight`, and the value that draws a light rather than nothing. The package defaults
/// it to `_Disable`, whose shader writes no light at all.
const GET_DIRECTIONAL_LIGHT: u32 = 0x8115_916d;
const GET_DIRECTIONAL_LIGHT_ENABLE: u32 = 0x51ed_d496;

/// `SpecularLighting`, and the value that works a specular out rather than moving nought into the
/// target the composite reads it back from. A placed light's package defaults it to `_Disable`.
const SPECULAR_LIGHTING: u32 = 0x0d81_2fa4;
const SPECULAR_LIGHTING_ENABLE: u32 = 0xaba1_f498;

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
    Composite,
    CompositeBlended,
}

impl Pass {
    fn id(self) -> u32 {
        match self {
            Self::Depth => PASS_Z_OPAQUE,
            Self::Buffer => PASS_G_OPAQUE,
            Self::Blended => PASS_G_SEMITRANSPARENCY,
            Self::Lighting | Self::Lamp => PASS_LIGHTING_OPAQUE,
            Self::Fur => PASS_FUR,
            Self::Composite => PASS_COMPOSITE_OPAQUE,
            Self::CompositeBlended => PASS_COMPOSITE_SEMITRANSPARENCY,
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
/// differently. Every kind the game states other than a spot draws as a point.
#[derive(Clone, Copy)]
pub enum LampKind {
    Point,
    Spot,
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

/// The light a place stands in, as `g_AmbientParamArray` holds one entry of it.
///
/// Each set of harmonics is three rows a shader dots against a normal and a one. A zone states its
/// own per time of day in the `.amb` its `EnvLocation` names, the sky's own come out of
/// `skylight.amb`, and `scale` is what a zone's `.envb` calls `ambient_light_scale`. Nothing in any
/// file states the rest of the entry.
#[derive(Clone, Copy)]
pub struct Ambient {
    pub sky: [Vec4; 3],
    /// What the sky's harmonics are taken back up by.
    pub sky_scale: f32,
    pub light: [Vec4; 3],
    pub scale: f32,
    /// How the ambient fades with depth: a scale, a bias and a floor.
    pub fade: Vec3,
    /// What a reflection is scaled and biased by, and how much of it reaches the frame. A scale of
    /// nought against a bias of one leaves the ambient where it was: the shader blends the two by
    /// the reflection's own alpha, so a bias of nought would take the ambient with it.
    pub reflection: Vec3,
    /// The roughness a reflection is sampled at, which the shader offsets by a tenth.
    pub roughness: f32,
    /// What the ambient is mixed toward, and how far that mix reaches.
    pub haze: Vec4,
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
            reflection: Vec3::new(1.0, 0.0, 0.0),
            roughness: 0.0,
            haze: Vec4::W,
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
        Vec4::new(
            coefficients[3],
            coefficients[1],
            coefficients[2],
            coefficients[0],
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

/// What the engine decides rather than the files. Everything a constant buffer holds that is not the
/// material's own comes from here, so a field that has to be reconstructed is reconstructed once.
#[derive(Clone, Copy)]
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
    /// The colours the character was made with.
    pub customize: Customize,
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
            customize: Customize::default(),
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
    parts.push(selector(&[package.technique_subview()[0], subview]));
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
        let (vs, ps) = pair(&package, held.shader_keys(), set, pass.id(), subview)
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
    pub fn screen(bytes: &[u8], pass: Pass, attachments: usize) -> Result<Self, String> {
        let package = ShaderPackage::parse(bytes).map_err(|why| why.to_string())?;
        // A key a package does not declare is never looked up, so one set serves every package here.
        let set = [
            (GET_DIRECTIONAL_LIGHT, GET_DIRECTIONAL_LIGHT_ENABLE),
            (SPECULAR_LIGHTING, SPECULAR_LIGHTING_ENABLE),
        ];
        let (vs, ps) = pair(&package, &[], &set, pass.id(), SUB_VIEW_MAIN)
            .ok_or("this package reaches no such pass")?;
        Self::assemble(&package, bytes, (vs, ps), None, pass, 0, attachments)
    }

    /// Translates one member of the game's post chain. A `.shcd` holds one shader and no node table,
    /// so the file is the variant and there is nothing to select; what it wants is a screen-wide
    /// draw of the vertex shader given, and a frame in the range a screen holds, since the pass that
    /// grades one saturates what it reads before it reads its table.
    pub fn posteffect(bytes: &[u8], vertex: &str) -> Result<Self, String> {
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
                fixed: (name == TONE_MAP_PARAM).then(|| {
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
            ambient(&scene.ambient, glam::Mat3::from_mat4(view), &mut out);
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
        put(INSTANCE, "m_MulColor", vec![1.0; 4]);
        // What a strand's length is scaled by, which a character's buffer pass packs into the fifth
        // target's alpha and the fur pass marches along the flow. The engine drives it per draw and
        // no file states one, so it goes in at the identity; nought is what leaves the march at
        // nothing however long the parameter file states the fur. The first lane alone: nothing
        // reads the rest of the register.
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
        // pixel, and the fade is off here: the scale is cubed and clamped, so a constant one leaves
        // it alone. A lamp is clipped at the square of its own reach and falls off as `w` over the
        // distance, which the ramp the light is read off then shapes. Only a spot's own shader reads
        // `y`, and the sun's reads the lane whatever is in it.
        let reach = lamp.reach();
        let cone = match pass {
            Pass::Lamp => lamp.cone,
            _ => 0.0,
        };
        put(
            light,
            "m_Attenuation",
            match pass {
                Pass::Composite | Pass::CompositeBlended => vec![0.0, 0.0, 1.0, 0.0],
                _ => vec![0.0, cone, 1.0 / (reach * reach), reach],
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
fn ambient(held: &Ambient, axes: glam::Mat3, out: &mut [u8]) {
    if out.len() < 8 {
        return;
    }
    let turned = |row: &Vec4| (axes * row.truncate()).extend(row.w);
    // The count reads as a whole number rather than as the float that would print the same.
    out[..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..8].copy_from_slice(&held.sky_scale.to_le_bytes());
    for (at, row) in held.sky.iter().enumerate() {
        write(out, 1 + at, &turned(row).to_array());
    }
    entry(held, axes, out, 4);
    // No bounding shape, so the entry covers the frame rather than a room.
    write(out, 14, &[0.0, 0.0, 0.0, 0.0]);
    write(out, 15, &[0.0, 1.0, 0.0, 0.0]);
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
            held.roughness,
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

    use super::{Ambient, JOINT, ROW, SHADER_TYPE, ambient, joints, selector, shader_types};

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
            roughness: 15.0,
            haze: Vec4::ZERO,
        };
        let mut out = vec![0u8; 16 * 16];
        ambient(&held, Mat3::IDENTITY, &mut out);
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

    /// The row is dotted against a normal and a one, and the file runs constant, `y`, `z`, `x`.
    #[test]
    fn a_harmonic_row_puts_the_constant_last() {
        let row = Ambient::row(&[1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(row, Vec4::new(4.0, 2.0, 3.0, 1.0));
        assert_eq!(row.dot(Vec4::new(0.0, 1.0, 0.0, 1.0)), 3.0);
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
