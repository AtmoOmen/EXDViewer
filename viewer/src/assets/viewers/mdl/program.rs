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
use ironworks::file::mtrl;
use ironworks::file::shpk::{self, ShaderPackage, Stage};

use super::material::Material;

/// The passes a model is drawn with.
const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const PASS_G_SEMITRANSPARENCY: u32 = 0x6006_067f;
const PASS_Z_OPAQUE: u32 = 0xe412_a2d4;
const PASS_LIGHTING_OPAQUE: u32 = 0xfbde_0a8f;
const PASS_COMPOSITE_OPAQUE: u32 = 0x955c_0b73;
const PASS_COMPOSITE_SEMITRANSPARENCY: u32 = 0xc885_bbd3;

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
pub const COMPOSITE: &str = "shader/sm5/shpk/bg_composite.shpk";

/// `GetDirectionalLight`, and the value that draws a light rather than nothing. The package defaults
/// it to `_Disable`, whose shader writes no light at all.
const GET_DIRECTIONAL_LIGHT: u32 = 0x8115_916d;
const GET_DIRECTIONAL_LIGHT_ENABLE: u32 = 0x51ed_d496;

/// Records of `g_ShaderTypeParameter`, which `SV_Target.w` indexes as `(32 + type) / 255`.
const SHADER_TYPES: usize = 256;

/// Dwords of one `g_ShaderTypeParameter` record.
const SHADER_TYPE: usize = 32;

/// Dwords in a row of the texture a structured buffer is read through, which the backend fixes so
/// that a shader and whatever fills the texture agree without either having to say so.
pub const ROW: usize = hlsl::glsl::ROW as usize;

/// Dwords of one joint's transform: four columns of three floats, densely packed.
const JOINT: usize = 12;

/// The buffer a drawing package reads one record of per object drawn.
const INSTANCING: &str = "g_InstancingData";

/// Which pass of the node to take. `Lighting` and `Lamp` take the same one: the sun and a placed
/// light are separate packages reading one buffer differently.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Depth,
    Buffer,
    Blended,
    Lighting,
    Lamp,
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
}

impl Default for Lamp {
    fn default() -> Self {
        Self {
            placement: Mat4::IDENTITY,
            min: Vec3::splat(-1.0),
            max: Vec3::ONE,
            color: Vec3::ONE,
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
            sky: [Vec4::ZERO; 3],
            sky_scale: 1.0,
            light: [Vec4::new(0.0, 0.0, 0.0, 0.12); 3],
            scale: 1.0,
            fade: Vec3::new(0.0, 1.0, 0.0),
            reflection: Vec3::new(0.0, 1.0, 0.0),
            roughness: 0.0,
            haze: Vec4::W,
        }
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
    parts.push(selector(&[package.subview_defaults()[0], subview]));
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
    let held = dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
        .find_map(|chunk| match chunk.parse() {
            dxbc::chunks::ChunkData::Shader(program) => Some(program),
            _ => None,
        })?;
    Some((held, blob))
}

/// What this shader's registers are called, and what its signatures declare.
fn names(package: &ShaderPackage, index: u32, blob: &[u8]) -> hlsl::Names {
    use dxbc::chunks::ChunkData;

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
    for chunk in dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
    {
        let (into, signature) = match chunk.parse() {
            ChunkData::InputSignature(signature) => (&mut names.inputs, signature),
            ChunkData::OutputSignature(signature) => (&mut names.outputs, signature),
            _ => continue,
        };
        for element in &signature.elements {
            into.entry(element.register).or_insert_with(|| {
                hlsl::Semantic::new(
                    &element.semantic_name,
                    element.semantic_index,
                    element.component_type,
                    element.mask,
                )
            });
        }
    }
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
        // The package defaults `GetDirectionalLight` to the shader that writes no light at all.
        let set = [(GET_DIRECTIONAL_LIGHT, GET_DIRECTIONAL_LIGHT_ENABLE)];
        let (vs, ps) = pair(&package, &[], &set, pass.id(), SUB_VIEW_MAIN)
            .ok_or("this package reaches no such pass")?;
        Self::assemble(&package, bytes, (vs, ps), None, pass, 0, attachments)
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
                buffers.push(Buffer {
                    members: described.get(&name).cloned().unwrap_or_default(),
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
        put("g_InstanceParameter", "m_MulColor", vec![1.0; 4]);
        put("g_ModelParameter", "m_Params", vec![1.0; 4]);
        // What skin showing through a stocking is multiplied by, which is not the light's own color
        // of the same name.
        put("g_SkinMaterialParameter", "m_DiffuseColor", vec![1.0; 3]);

        // The colors a character was made with, which no file a model names holds. Each is what an
        // albedo is multiplied by or mixed toward, so white leaves the texture's own color where it
        // is and nought takes hair and eyes to black. A lip tint is left alone: its own alpha is the
        // weight it carries. The last lane of the two hair colors is where a decal is read from.
        let customize = "g_CustomizeParameter";
        for name in ["m_SkinColor", "m_MainColor", "m_LeftColor", "m_RightColor"] {
            put(customize, name, vec![1.0; 4]);
        }
        put(customize, "m_MeshColor", vec![1.0, 1.0, 1.0, 0.0]);
        put(customize, "m_OptionColor0", vec![1.0; 3]);

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
        put(
            light,
            "m_Direction",
            (axes * scene.light).normalize_or_zero().to_array().to_vec(),
        );
        let lamp = scene.lamp;
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
        // it alone. A lamp is clipped at the square of its own reach and scaled by it.
        let reach = lamp.reach();
        put(
            light,
            "m_Attenuation",
            match pass {
                Pass::Composite | Pass::CompositeBlended => vec![0.0, 0.0, 1.0, 0.0],
                _ => vec![0.0, 0.0, 1.0 / (reach * reach), reach],
            },
        );
        put(light, "m_LightFadeValueStatic", vec![1.0]);
        put(light, "m_LightFadeValueDynamic", vec![1.0]);

        // A lamp is drawn as the volume it reaches: its own vertex shader clamps a unit box to the
        // extents the zone clips it against and then projects, so the transform carries the light's
        // whole reach and the extents cut the box back out of it.
        let volume = lamp.placement * Mat4::from_scale(Vec3::splat(reach));
        let (min, max) = lamp.clip();
        put(
            light,
            "m_Position",
            (view * lamp.placement * Vec3::ZERO.extend(1.0))
                .to_array()
                .to_vec(),
        );
        put(light, "m_ClipMin", min.to_array().to_vec());
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
/// A joint's transform is the object's own, composed with what the pose moved that bone by; a model
/// stands in the pose it is stored in until something animates it, and that composes to the object's
/// own transform for every bone alike.
pub fn joints(count: usize, transform: Mat4) -> Vec<u32> {
    let columns = transform.to_cols_array();
    let rows = (count.max(1) * JOINT).div_ceil(ROW);
    let mut out = vec![0u32; rows * ROW];
    for at in 0..count {
        for column in 0..4 {
            for lane in 0..3 {
                out[at * JOINT + column * 3 + lane] = columns[column * 4 + lane].to_bits();
            }
        }
    }
    out
}

/// The table `SV_Target.w` indexes, as the dwords of the texture standing in for a structured
/// buffer. A record's first field is the lighting model the shader branches on; nothing in any file
/// says what the rest hold, so they are left at nought, which is the branch a plain surface takes.
///
/// Every index a G pass can write has a record, since the one it writes is `(32 + type) / 255` and
/// what a material makes of that is the material's own business.
pub fn shader_types() -> Vec<u32> {
    let rows = (SHADER_TYPES * SHADER_TYPE).div_ceil(ROW);
    vec![0u32; rows * ROW]
}

/// The color table as the game's own shaders read it: the rows exactly as the material states them,
/// four halfs to a texel. Answers the halfs, the texels a row takes, and the rows.
pub fn table(held: &mtrl::ColorTable) -> Option<(&[u16], usize, usize)> {
    let rows = held.rows();
    let raw = held.raw();
    if rows == 0 || !raw.len().is_multiple_of(rows * 4) {
        return None;
    }
    Some((raw, raw.len() / rows / 4, rows))
}

#[cfg(test)]
mod test {
    use glam::{Mat3, Mat4, Vec3, Vec4};

    use super::{Ambient, JOINT, ROW, ambient, joints, selector};

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
        let held = joints(2, Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0)));
        assert_eq!(held.len(), ROW);
        let value = |lane: usize| f32::from_bits(held[lane]);
        assert_eq!(
            (0..JOINT).map(value).collect::<Vec<_>>(),
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 4.0, 5.0, 6.0]
        );
        assert_eq!(&held[JOINT..JOINT * 2], &held[..JOINT]);
    }
}
