//! An effect drawn with the shaders the game would draw it with.
//!
//! The apricot packages are a forward pipeline of their own: one pass, two targets, their own light
//! model, and no material keys at all. What a drawing package reads off an `.mtrl` a particle states
//! itself, so the texture sets it names, the arithmetic it combines them with and the lights it takes
//! are all scene keys, and the node is picked at translation time.

use std::collections::{BTreeSet, HashMap};

use glam::{Mat4, Vec3, Vec4};
use ironworks::file::shpk::{self, ShaderPackage, Stage};

/// The packages a particle is drawn with, by what it draws as.
pub const SHAPE: &str = "shader/sm5/shpk/apricot_shape.shpk";
pub const MODEL: &str = "shader/sm5/shpk/apricot_model.shpk";

/// The only pass any apricot package carries.
const PASS_0: u32 = 0xc5a5_389c;

/// UV sets a particle may carry, and the registers one takes of the transform buffer.
pub const UV_SETS: usize = 4;
pub const UV_REGISTERS: usize = 2;

/// Two rows of an affine on each uv set, read as `dot(vec3(uv, 1), row.xyw)`, transforming nothing.
/// The coordinate they take is centered on the texture's middle, so the half is what puts it back.
pub const UV_IDENTITY: [[f32; 4]; UV_SETS * UV_REGISTERS] = [
    [1.0, 0.0, 0.0, 0.5],
    [0.0, 1.0, 0.0, 0.5],
    [1.0, 0.0, 0.0, 0.5],
    [0.0, 1.0, 0.0, 0.5],
    [1.0, 0.0, 0.0, 0.5],
    [0.0, 1.0, 0.0, 0.5],
    [1.0, 0.0, 0.0, 0.5],
    [0.0, 1.0, 0.0, 0.5],
];

/// What the sprite packages read an integer attribute as: the shader scales by a thousandth.
pub const FIXED: f32 = 1000.0;

/// Which vertex field a semantic reads from. A particle's geometry is one layout, so this is the
/// whole of it: the shape packages take a stream the viewer has already placed in the world, and the
/// model packages take the effect's own mesh with a transform beside it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Position,
    Normal,
    Tangent,
    Color,
    /// The first two UV sets, and the last two.
    Uv01,
    Uv23,
    Extra,
}

pub struct Attribute {
    pub location: u32,
    pub field: Field,
    /// Whether the signature declares an integer component type, which takes an integer pointer.
    pub integer: bool,
}

pub struct Texture {
    pub name: String,
    /// The package's own resource id, which is what a role is recognized by.
    pub id: u32,
}

pub struct Buffer {
    pub name: String,
    members: Vec<hlsl::layout::Member>,
    registers: u32,
}

/// What the engine decides rather than the file.
#[derive(Clone, Copy)]
pub struct Scene {
    pub view: Mat4,
    pub projection: Mat4,
    /// The frame in pixels.
    pub size: (f32, f32),
    /// Which way the sun comes from, in world space.
    pub light: Vec3,
    pub diffuse: Vec3,
    pub ambient: Vec3,
    /// The effect's own `SPFR`, which only the apricot_model technique that samples depth reads,
    /// as the range a soft particle fades over rather than the screen size the register otherwise
    /// holds. Zero where unset, which divides out to no softening.
    pub fade_range: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            view: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            size: (1.0, 1.0),
            light: Vec3::Y,
            diffuse: Vec3::ONE,
            ambient: Vec3::splat(0.12),
            fade_range: 0.0,
        }
    }
}

/// The rim ramp a model particle draws with: the shader lerps `begin` into `end` by
/// `pow(dot(view, normal), power)`, carrying alpha with the colour.
#[derive(Clone, Copy)]
pub struct Rim {
    pub power: f32,
    pub begin: [f32; 4],
    pub end: [f32; 4],
}

impl Default for Rim {
    /// Equal ends leave the lerp an identity, which is what a sprite draws with.
    fn default() -> Self {
        Self {
            power: 1.0,
            begin: [1.0; 4],
            end: [1.0; 4],
        }
    }
}

/// One object drawn, as `g_VS_PerInstanceParameters` holds one.
#[derive(Clone, Copy)]
pub struct Instance {
    pub transform: Mat4,
    pub color: Vec4,
    pub rim: Rim,
    /// How far towards the camera the depth is pulled, which is what keeps an effect off a surface
    /// it sits against.
    pub depth_offset: f32,
    /// What each of the object's uv sets does to a texture coordinate, two registers a set.
    pub uv: [[f32; 4]; UV_SETS * UV_REGISTERS],
    /// How much of the color texture's own color and alpha reach the particle's, which the package
    /// lerps the sampled texel towards white by.
    pub calculate: [f32; 2],
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            transform: Mat4::IDENTITY,
            color: Vec4::ONE,
            rim: Rim::default(),
            depth_offset: 0.0,
            uv: UV_IDENTITY,
            calculate: [1.0; 2],
        }
    }
}

/// Everything one draw of one particle needs.
pub struct Program {
    pub vertex: String,
    pub fragment: String,
    pub attributes: Vec<Attribute>,
    pub textures: Vec<Texture>,
    pub buffers: Vec<Buffer>,
    /// Every target the shader declares, in register order.
    pub outputs: Vec<u32>,
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

/// The id the game gives a key or a value, which is the crc of its own name.
pub fn id(name: &str) -> u32 {
    shaders::names::hash(name.as_bytes())
}

/// What a group of keys resolves to: the draw's own value where it sets the category, and the
/// package's default otherwise.
fn values(keys: &[shpk::Key], set: &[(u32, u32)]) -> Vec<u32> {
    keys.iter()
        .map(|key| {
            set.iter()
                .find(|(held, _)| *held == key.id())
                .map(|(_, value)| *value)
                .unwrap_or_else(|| key.default_value())
        })
        .collect()
}

/// The shaders these keys draw the pass with, as indices into the package's own list.
fn pair(package: &ShaderPackage, set: &[(u32, u32)]) -> Option<(u32, u32)> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, set)))
    .collect();
    parts.push(selector(&package.technique_subview()));
    let id = selector(&parts);

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
    let held = node.passes().iter().find(|held| held.id() == PASS_0)?;
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

/// The pair these keys draw with. A package carries only the combinations it was built with, so a
/// set reaching no node gives keys up one at a time rather than all at once; they arrive in the
/// order they matter in, so the last is the first to go.
///
/// No particle field names `ComputeSoftParticleType`: apricot compiles it as an all-or-nothing
/// split across whole texture combinations rather than a per-particle toggle, so a combination
/// that only exists with it enabled reaches nothing at the package's own default (`Disable`) and
/// the truncation above lands on an unrelated node instead. Tried whole and un-truncated first,
/// since every combination observed compiles at exactly one of the two states.
fn resolve(package: &ShaderPackage, set: &[(u32, u32)]) -> Option<(u32, u32)> {
    let mut soft = Vec::with_capacity(set.len() + 1);
    soft.push((
        id("ComputeSoftParticleType_Table"),
        id("ComputeSoftParticleType_Enable"),
    ));
    soft.extend_from_slice(set);
    pair(package, &soft).or_else(|| {
        (0..=set.len())
            .rev()
            .find_map(|held| pair(package, &set[..held]))
    })
}

fn program<'a>(
    package: &ShaderPackage,
    bytes: &'a [u8],
    index: u32,
) -> Option<(dxbc::shex::Program, &'a [u8])> {
    let shader = package.shaders().get(index as usize)?;
    let start = package
        .blobs_offset()
        .checked_add(usize::try_from(shader.blob_offset()).ok()?)?;
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

fn field(semantic: &str) -> Option<Field> {
    Some(match semantic.to_ascii_uppercase().as_str() {
        "POSITION" => Field::Position,
        "NORMAL" => Field::Normal,
        "TANGENT" => Field::Tangent,
        "COLOR" => Field::Color,
        "TEXCOORD" => Field::Uv01,
        "TEXCOORD1" => Field::Uv23,
        "TEXCOORD2" => Field::Extra,
        "TEXCOORD3" => Field::Extra,
        _ => return None,
    })
}

/// What the sprite packages put where the model packages put a normal and a tangent: the color and
/// the uv sets, both as integers the shader scales.
fn sprite_field(semantic: &str) -> Option<Field> {
    Some(match semantic.to_ascii_uppercase().as_str() {
        "POSITION" => Field::Position,
        "TEXCOORD" => Field::Color,
        "TEXCOORD1" => Field::Uv01,
        "TEXCOORD2" => Field::Uv23,
        "TEXCOORD3" => Field::Extra,
        _ => return None,
    })
}

impl Program {
    /// Translates the pair these keys draw with. The keys are the whole of the variation: apricot
    /// declares no material keys, so a particle's own texture sets and lights are scene keys.
    pub fn build(bytes: &[u8], set: &[(u32, u32)], sprite: bool) -> Result<Self, String> {
        let package = ShaderPackage::parse(bytes).map_err(|why| why.to_string())?;
        let (vs, ps) = resolve(&package, set).ok_or("these keys reach no node")?;
        let (vertex, vs_blob) =
            program(&package, bytes, vs).ok_or("no vertex shader in the blob")?;
        let (fragment, ps_blob) =
            program(&package, bytes, ps).ok_or("no pixel shader in the blob")?;
        let vs_names = names(&package, vs, vs_blob);
        let ps_names = names(&package, ps, ps_blob);
        let mut described = HashMap::new();
        layouts(vs_blob, &mut described);
        layouts(ps_blob, &mut described);

        // A uniform block has to be spelled identically in both stages or the program will not link.
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
        let vs_options = hlsl::glsl::Options {
            targets: Vec::new(),
            extents: extents.clone(),
        };
        let ps_options = hlsl::glsl::Options {
            targets: outputs.clone(),
            extents,
        };
        let read = |program, names, options| {
            hlsl::glsl(program, names, hlsl::Reading::Plain, options)
                .lines
                .join("\n")
        };

        let held = match sprite {
            true => sprite_field,
            false => field,
        };
        let mut attributes: Vec<Attribute> = vs_names
            .inputs
            .iter()
            .filter_map(|(register, entry)| {
                Some(Attribute {
                    location: *register,
                    field: held(&entry.name)?,
                    integer: entry.kind.starts_with("int") || entry.kind.starts_with("uint"),
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
            for (slot, _, name) in hlsl::glsl::textures(program, names) {
                let Some(resource) = resources.iter().find(|held| held.slot() == slot) else {
                    continue;
                };
                if textures.iter().all(|held| held.name != name) {
                    textures.push(Texture {
                        name,
                        id: resource.id(),
                    });
                }
            }
        }

        let mut buffers: Vec<Buffer> = Vec::new();
        for (program, names) in [(&vertex, &vs_names), (&fragment, &ps_names)] {
            for (name, registers) in hlsl::glsl::extents(program, names) {
                if buffers.iter().any(|held| held.name == name) {
                    continue;
                }
                buffers.push(Buffer {
                    members: described.get(&name).cloned().unwrap_or_default(),
                    name,
                    registers,
                });
            }
        }

        Ok(Self {
            vertex: read(&vertex, &vs_names, &vs_options),
            fragment: read(&fragment, &ps_names, &ps_options),
            attributes,
            textures,
            buffers,
            outputs,
        })
    }
}

impl Buffer {
    /// The bytes this buffer holds, filled by field name off the reflection. The camera and the frame
    /// are the scene's; the transform and the tint are the particle's; what nothing names stays zero.
    pub fn fill(&self, scene: &Scene, instance: &Instance) -> Vec<u8> {
        let span = self
            .members
            .iter()
            .map(|member| member.offset + member.size)
            .max()
            .unwrap_or(0)
            .max(self.registers * 16)
            .max(16);
        let mut out = vec![0u8; span.div_ceil(16) as usize * 16];

        // A matrix reads as its columns: apricot's vertex shaders scale each register by one lane of
        // the position and add, where a drawing package dots the position against each register.
        let columns = |matrix: Mat4, count: usize| -> Vec<f32> {
            matrix.to_cols_array()[..count * 4].to_vec()
        };
        let view_projection = scene.projection * scene.view;
        let eye = scene.view.inverse().w_axis.truncate();

        // The buffers whose reflection names nothing are written by register.
        match self.name.as_str() {
            "g_VS_ViewProjectionMatrix" => {
                write_rows(&mut out, 0, &columns(view_projection, 4));
                return out;
            }
            "g_VS_ProjectionInverseMatrix" => {
                write_rows(&mut out, 0, &columns(scene.projection.inverse(), 4));
                return out;
            }
            "g_PS_ViewProjectionInverseMatrix" => {
                write_rows(&mut out, 0, &columns(view_projection.inverse(), 4));
                return out;
            }
            // Two rows of an affine transform on each uv set, read as `dot(uv1, row.xyw)`.
            "g_PS_UvTransform" => {
                for (register, row) in instance.uv.iter().enumerate() {
                    write_rows(&mut out, register, row);
                }
                return out;
            }
            // A tone map the viewer has no curve for, off: the strength is nought and the exposure
            // one, so the shader's own lerp leaves the color alone.
            "g_ToneMapParameter" => {
                write_rows(&mut out, 0, &[0.0, 0.0, 0.0, 1.0]);
                return out;
            }
            _ => {}
        }

        let mut put = |name: &str, values: Vec<f32>| {
            let Some(member) = self.members.iter().find(|held| held.name == name) else {
                return;
            };
            for (at, value) in values.iter().enumerate() {
                let offset = member.offset as usize + at * 4;
                if offset + 4 <= out.len() {
                    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
        };

        put("WorldMatrix", columns(instance.transform, 4));
        put("Parameters", vec![instance.depth_offset, 0.0, 0.0, 0.0]);
        put("Color", instance.color.to_array().to_vec());

        // The fourth lane is unread by every technique but the one soft-particle variant that
        // samples depth, which reads it as the fade range rather than 1/height: the RDEF names the
        // whole register `ScreenSize` regardless, so nothing else can tell the two apart by name.
        let (width, height) = (scene.size.0.max(1.0), scene.size.1.max(1.0));
        put("ScreenSize", vec![width, height, 1.0 / width, scene.fade_range]);
        put("ScreenRealSize", vec![width, height]);
        put("ModulateColor", vec![1.0; 4]);
        put("FogParam", vec![0.0, 0.0, 0.0, 0.0]);
        put("CameraParam", vec![eye.x, eye.y, eye.z, 1.0]);

        // The axis the rim ramp is measured against, which the modifier below turns into a real
        // per-pixel view vector by subtracting the surface it is read at.
        put("EyePosition", vec![eye.x, eye.y, eye.z, 1.0]);
        put(
            "FresnelParameter",
            [
                eye.to_array().to_vec(),
                vec![instance.rim.power],
                instance.rim.begin.to_vec(),
                instance.rim.end.to_vec(),
            ]
            .concat(),
        );
        put("WorldPosition", vec![0.0, 0.0, 0.0, 1.0]);
        put("ViewportPosition", vec![0.0, 0.0, width, height]);

        // Scaling the world position out of the axis above, so it reads as `eye - surface`.
        put("FresnelAxisModifier", vec![1.0]);
        put("CalculateColor", vec![instance.calculate[0]]);
        put("CalculateAlpha", vec![instance.calculate[1]]);
        put("ApplyToneMap", vec![0.0]);
        put("BlendStateType", vec![0.0]);

        // The depth of field, off: every depth answers with the same texel of the lookup.
        put("lutStartZ", vec![0.0]);
        put("invRange", vec![0.0]);

        // The light model apricot carries itself, which is not the deferred graph's.
        let light = scene.light.normalize_or_zero();
        put(
            "Scene_AmbientColor",
            scene.ambient.extend(1.0).to_array().to_vec(),
        );
        put(
            "AmbientColor",
            scene.ambient.extend(1.0).to_array().to_vec(),
        );
        for name in ["DirectionalLight_Direction", "DirectionalLightDirection"] {
            put(name, light.extend(0.0).to_array().to_vec());
        }
        for name in ["DirectionalLight_Color", "DirectionalLightColor"] {
            put(name, scene.diffuse.extend(1.0).to_array().to_vec());
        }
        put("UseBgAmbient", vec![0.0]);
        put("UniformShadingValue", vec![1.0]);
        put("HalfShadingValue", vec![1.0, 1.0]);
        out
    }
}

/// One register of a buffer written as floats.
fn write_rows(out: &mut [u8], register: usize, values: &[f32]) {
    for (at, value) in values.iter().enumerate() {
        let offset = register * 16 + at * 4;
        if offset + 4 <= out.len() {
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod test {
    use super::{Buffer, Instance, Rim, Scene, id, selector};

    fn buffer(name: &str, members: &[(&str, u32, u32)], registers: u32) -> Buffer {
        Buffer {
            name: name.to_owned(),
            members: members
                .iter()
                .map(|(name, offset, size)| hlsl::layout::Member {
                    name: (*name).to_owned(),
                    offset: *offset,
                    size: *size,
                    kind: "float4".to_owned(),
                })
                .collect(),
            registers,
        }
    }

    fn lane(bytes: &[u8], at: usize) -> f32 {
        f32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    /// `g_PS_ModelSpecificParameters` holds the power in the axis register's last lane and the two
    /// ends of the lerp in the two after it, alpha with them.
    #[test]
    fn the_rim_ramp_reaches_the_registers_the_package_lerps() {
        let held = buffer(
            "g_PS_ModelSpecificParameters",
            &[("EyePosition", 0, 16), ("FresnelParameter", 16, 48)],
            6,
        );
        let scene = Scene::default();

        let bytes = held.fill(&scene, &Instance::default());
        assert_eq!(bytes[32..48], bytes[48..64], "a sprite has to lerp nowhere");

        let instance = Instance {
            rim: Rim {
                power: 3.0,
                begin: [1.0, 1.0, 1.0, 0.0],
                end: [0.25, 0.5, 0.75, 1.0],
            },
            ..Instance::default()
        };
        let bytes = held.fill(&scene, &instance);
        assert_eq!(lane(&bytes, 28), 3.0);
        assert_eq!(lane(&bytes, 44), 0.0);
        assert_eq!(lane(&bytes, 48), 0.25);
        assert_eq!(lane(&bytes, 60), 1.0);
    }

    /// The axis is only a view vector where the world position is scaled out of it whole.
    #[test]
    fn the_fresnel_axis_carries_the_surface_out_of_the_eye() {
        let held = buffer(
            "g_PS_InstanceExtraParameters",
            &[("FresnelAxisModifier", 0, 4)],
            2,
        );
        assert_eq!(
            lane(&held.fill(&Scene::default(), &Instance::default()), 0),
            1.0
        );
    }

    /// `CalculateColor` and `CalculateAlpha` sit in the two lanes after the axis, and a particle
    /// whose color texture carries no color states the first of them at nought.
    #[test]
    fn the_calculate_ratios_reach_the_lanes_after_the_axis() {
        let held = buffer(
            "g_PS_InstanceExtraParameters",
            &[
                ("FresnelAxisModifier", 0, 4),
                ("CalculateColor", 4, 4),
                ("CalculateAlpha", 8, 4),
            ],
            2,
        );
        let instance = Instance {
            calculate: [0.0, 0.25],
            ..Instance::default()
        };
        let bytes = held.fill(&Scene::default(), &instance);
        assert_eq!(lane(&bytes, 4), 0.0);
        assert_eq!(lane(&bytes, 8), 0.25);
    }

    #[test]
    fn the_selector_is_a_polynomial_in_thirty_one() {
        assert_eq!(selector(&[]), 0);
        assert_eq!(selector(&[7]), 7);
        assert_eq!(selector(&[1, 1]), 32);
        assert_eq!(selector(&[0, 0, 1]), 961);
    }

    /// The key names the effect viewer asks a package for have to be the ones the package declares,
    /// and a package names them by the crc of the name alone.
    #[test]
    fn the_key_names_hash_to_the_ids_the_packages_carry() {
        assert_eq!(id("PASS_0"), 0xc5a5_389c);
        assert_eq!(id("TextureColor1_Table"), 0x3045_0f85);
        assert_eq!(id("UvSetCount_Table"), 0xfd7a_2be9);
    }
}
