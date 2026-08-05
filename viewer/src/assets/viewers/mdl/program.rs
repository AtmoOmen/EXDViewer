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

use glam::Mat4;
use ironworks::file::mtrl;
use ironworks::file::shpk::{self, ShaderPackage, Stage};

use super::material::Material;

/// The passes a model is drawn with, and the subview they are drawn under.
const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const PASS_Z_OPAQUE: u32 = 0xe412_a2d4;
const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;

/// Dwords in a row of the texture a structured buffer is read through, which the backend fixes so
/// that a shader and whatever fills the texture agree without either having to say so.
pub const ROW: usize = hlsl::glsl::ROW as usize;

/// Dwords of one joint's transform: four columns of three floats, densely packed.
const JOINT: usize = 12;

/// Which pass of the node to take.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Depth,
    Buffer,
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

/// One vertex attribute, as the shader's own input signature asks for it.
pub struct Attribute {
    pub location: u32,
    pub field: Field,
    /// Whether the signature declares an integer component type, which takes an integer pointer: a
    /// float one feeds a `uvec4` attribute values nothing checks.
    pub integer: bool,
}

/// A texture the shader samples, named as GLSL has it and identified as the material names it.
pub struct Texture {
    pub name: String,
    /// The package's own resource id, which is the crc a material's samplers use.
    pub id: u32,
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
) -> Option<(u32, u32)> {
    let mut parts: Vec<u32> = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ]
    .iter()
    .map(|keys| selector(&values(keys, material, set)))
    .collect();
    parts.push(selector(&[package.subview_defaults()[0], SUB_VIEW_MAIN]));
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
        put(param.byte_offset() as usize, &values[..lanes.min(values.len())]);
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
        target: usize,
        attachments: usize,
    ) -> Result<Self, String> {
        let package = ShaderPackage::parse(bytes).map_err(|why| why.to_string())?;
        let held = material.held();
        let want = match pass {
            Pass::Depth => PASS_Z_OPAQUE,
            Pass::Buffer => PASS_G_OPAQUE,
        };
        let (vs, ps) = pair(&package, held.shader_keys(), set, want)
            .ok_or("this material's keys reach no such pass")?;
        let (vertex, vs_blob) =
            program(&package, bytes, vs).ok_or("no vertex shader in the blob")?;
        let (fragment, ps_blob) =
            program(&package, bytes, ps).ok_or("no pixel shader in the blob")?;
        let vs_names = names(&package, vs, vs_blob);
        let ps_names = names(&package, ps, ps_blob);
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

        let parameters = parameters(&package, held);
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
                let fixed = (name == "g_MaterialParameter").then(|| parameters.clone());
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
        })
    }

    /// Where in this reading's attachments the wanted target landed.
    pub fn attachment(&self, target: usize) -> Option<usize> {
        let register = self.outputs.get(target)?;
        self.targets.iter().position(|held| held == register)
    }
}

impl Buffer {
    /// The bytes this buffer holds, filled by field name off the reflection. What the files decide
    /// is worked out once; everything else is the camera this viewer controls, and whatever nothing
    /// names stays zero.
    pub fn fill(&self, view: Mat4, projection: Mat4, model: Mat4) -> Vec<u8> {
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
        let world_view = view * model;
        let view_projection = projection * view;
        // Nothing here moves between frames, so every previous-frame matrix is the current one and
        // the motion vectors come out as nought.
        for name in ["m_ViewMatrix", "m_ViewMatrixPrev"] {
            put(name, rows(view, 3));
        }
        for name in [
            "m_InverseViewMatrix",
            "m_InverseViewMatrixPrev",
            "m_MainViewToWorldMatrix",
        ] {
            put(name, rows(view.inverse(), 3));
        }
        for name in ["m_ViewProjectionMatrix", "m_ViewProjectionMatrixPrev"] {
            put(name, rows(view_projection, 4));
        }
        for name in [
            "m_InverseViewProjectionMatrix",
            "m_InverseViewProjectionMatrixPrev",
        ] {
            put(name, rows(view_projection.inverse(), 4));
        }
        for name in [
            "m_ProjectionMatrix",
            "m_ProjectionMatrixPrev",
            "m_MainViewToProjectionMatrix",
        ] {
            put(name, rows(projection, 4));
        }
        for name in ["m_InverseProjectionMatrix", "m_InverseProjectionMatrixPrev"] {
            put(name, rows(projection.inverse(), 4));
        }
        for name in ["m_ProjToProjPrevMatrix", "m_ViewToViewPrevMatrix"] {
            put(name, rows(Mat4::IDENTITY, 4));
        }
        // The transform a vertex shader multiplies by before the projection alone, with nothing
        // between the two: it takes an object into view space rather than into the world. The buffer
        // holds this frame's and the last one's.
        put("g_WorldViewMatrix", {
            let mut held = rows(world_view, 3);
            held.extend(rows(world_view, 3));
            held
        });
        put("m_TransformMatrix", rows(world_view, 3));
        put("m_MulColor", vec![1.0; 4]);
        put("m_Param", vec![1.0; 4]);
        put("m_Params", vec![1.0; 4]);
        put("m_SkyVisibility", vec![1.0]);
        put("m_DitherAlpha", vec![1.0]);
        out
    }
}

/// The joint transforms a skinned shader reads, as the dwords of the texture standing in for a
/// structured buffer. Each is four columns of three floats, and each is the identity, which is what
/// poses a model into the bind pose it is stored in.
pub fn joints(count: usize) -> Vec<u32> {
    let rows = (count.max(1) * JOINT).div_ceil(ROW);
    let mut out = vec![0u32; rows * ROW];
    for at in 0..count {
        for lane in 0..3 {
            out[at * JOINT + lane * 4] = 1.0f32.to_bits();
        }
    }
    out
}

/// The color table as the game's own shaders read it: the rows exactly as the material states them,
/// four halfs to a texel. Answers the halfs, the texels a row takes, and the rows.
pub fn table(held: &mtrl::ColorTable) -> Option<(&[u16], usize, usize)> {
    let rows = held.rows();
    let raw = held.raw();
    if rows == 0 || raw.len() % (rows * 4) != 0 {
        return None;
    }
    Some((raw, raw.len() / rows / 4, rows))
}

#[cfg(test)]
mod test {
    use super::{JOINT, ROW, joints, selector};

    #[test]
    fn the_selector_is_a_polynomial_in_thirty_one() {
        assert_eq!(selector(&[]), 0);
        assert_eq!(selector(&[7]), 7);
        assert_eq!(selector(&[1, 1]), 32);
        assert_eq!(selector(&[0, 0, 1]), 961);
        assert_eq!(selector(&[u32::MAX, 2]), u32::MAX.wrapping_add(62));
    }

    /// Four columns of three floats each, the first three of which carry the diagonal.
    #[test]
    fn a_joint_is_four_columns_of_three() {
        let held = joints(2);
        assert_eq!(held.len(), ROW);
        let one = 1.0f32.to_bits();
        assert_eq!(&held[..JOINT], &[one, 0, 0, 0, one, 0, 0, 0, one, 0, 0, 0]);
        assert_eq!(&held[JOINT..JOINT * 2], &held[..JOINT]);
    }
}
