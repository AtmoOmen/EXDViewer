//! A package's own shaders, run.
//!
//! The node table picks the vertex and pixel shader a material would draw with, both are translated
//! to GLSL ES 3.00, and a generated mesh is drawn with them. One G-buffer channel goes on screen at
//! a time, so the fragment shader is emitted with that target alone: a shader declaring five outputs
//! will not link where the context has four draw buffers, which makes it a translation-time choice
//! rather than something a draw can mask.
//!
//! What is bound is the package's own account of itself. Material parameters come from the shpk's
//! defaults, and every other buffer is filled field by field off the reflection: a camera this
//! viewer controls goes into `g_CameraParameter`'s named matrices, an identity transform into
//! `g_InstancingData`, and everything nothing names stays zero. Textures are flat stand-ins.

use std::sync::{Arc, Mutex};

use egui::{RichText, Sense, vec2};
use glam::{Mat4, Vec3};
use glow::HasContext;
use ironworks::file::shpk::{self, ShaderPackage, Stage};

use super::Rendered;
use crate::assets::viewers::mdl::gpu::{Dead, bury, graveyard};

/// The pass to draw, and the subview it is drawn under.
const PASS_G_OPAQUE: u32 = 0x03ac_862e;
const SUB_VIEW_MAIN: u32 = 0xf43b_2f35;

/// Rings and segments of the generated mesh.
const RINGS: usize = 32;
const SEGMENTS: usize = 64;

/// Vertical field of view, and where the camera stands.
const FOV: f32 = 40.0_f32.to_radians();
const DISTANCE: f32 = 3.0;

/// What a stand-in texture answers with, by what the sampler's name says it holds.
const FLAT_NORMAL: [f32; 4] = [0.5, 0.5, 1.0, 1.0];
const MID_GREY: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

/// A vertex of the generated mesh, in the order the attributes are bound.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 4],
    normal: [f32; 4],
    tangent: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
}

/// Which semantic each of a vertex's fields answers to, and where it starts.
const FIELDS: [(&str, i32); 5] = [
    ("POSITION", 0),
    ("NORMAL", 16),
    ("BINORMAL", 32),
    ("TEXCOORD", 48),
    ("COLOR", 64),
];

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

/// The shaders a material with no key of its own would draw this pass with, as indices into the
/// package's own list.
fn pair(package: &ShaderPackage) -> Option<(u32, u32)> {
    let groups = [
        package.system_keys(),
        package.scene_keys(),
        package.material_keys(),
    ];
    let mut parts: Vec<u32> = groups
        .iter()
        .map(|keys| {
            let values: Vec<u32> = keys.iter().map(shpk::Key::default_value).collect();
            selector(&values)
        })
        .collect();
    parts.push(selector(&[package.technique_subview()[0], SUB_VIEW_MAIN]));
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
    let pass = node
        .passes()
        .iter()
        .find(|pass| pass.id() == PASS_G_OPAQUE)?;
    if pass.vertex() == shpk::NONE || pass.pixel() == shpk::NONE {
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
        base(Stage::Vertex) + pass.vertex(),
        base(Stage::Pixel) + pass.pixel(),
    ))
}

/// One shader's blob, and the program the disassembler read out of it.
fn program(
    package: &ShaderPackage,
    bytes: &[u8],
    index: u32,
) -> Option<(dxbc::shex::Program, usize)> {
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
    Some((held, start))
}

/// A buffer this shader binds, and the fields the reflection describes it with.
struct Buffer {
    name: String,
    registers: u32,
    members: Vec<hlsl::layout::Member>,
}

/// Everything one draw needs, worked out off the file rather than held on the card.
struct Built {
    vertex: String,
    fragment: String,
    buffers: Vec<Buffer>,
    /// Sampler uniform names, and what each stands in for.
    textures: Vec<(String, [f32; 4])>,
    /// Vertex attribute locations, by the semantic the mesh supplies.
    attributes: Vec<(&'static str, u32, i32)>,
    targets: Vec<String>,
}

/// What this shader's registers are called.
fn names(package: &ShaderPackage, index: u32, blob: &[u8]) -> hlsl::Names {
    use dxbc::chunks::ChunkData;

    let mut names = hlsl::Names::default();
    let Some(shader) = package.shaders().get(index as usize) else {
        return names;
    };
    let named = |resource: &shpk::Resource| package.name(resource).map(str::to_owned);
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

/// The buffer layouts a blob's reflection describes, by name.
fn layouts(blob: &[u8]) -> std::collections::HashMap<String, Vec<hlsl::layout::Member>> {
    let mut out = std::collections::HashMap::new();
    for chunk in dxbc::scan_dxbc(blob)
        .iter()
        .flat_map(|container| &container.chunks)
    {
        if let dxbc::chunks::ChunkData::Rdef(rdef) = chunk.parse() {
            for buffer in &rdef.constant_buffers {
                out.insert(buffer.name.to_string(), hlsl::layout::members(buffer));
            }
        }
    }
    out
}

/// Translate the pass's two shaders, sized so the pair declares every buffer alike.
fn build(package: &ShaderPackage, bytes: &[u8], target: u32) -> Result<Built, String> {
    let (vs, ps) = pair(package).ok_or("this package has no opaque G-buffer pass")?;
    let (vertex, vs_at) = program(package, bytes, vs).ok_or("no vertex shader in the blob")?;
    let (fragment, ps_at) = program(package, bytes, ps).ok_or("no pixel shader in the blob")?;
    let vs_names = names(package, vs, &bytes[vs_at..]);
    let ps_names = names(package, ps, &bytes[ps_at..]);

    // A uniform block has to be spelled identically in both stages or the program will not link,
    // and the two disagree on the extent of a shared buffer more often than not.
    let mut extents = hlsl::glsl::extents(&vertex, &vs_names);
    for (name, registers) in hlsl::glsl::extents(&fragment, &ps_names) {
        let held = extents.entry(name).or_insert(0);
        *held = (*held).max(registers);
    }

    let vs_options = hlsl::glsl::Options {
        targets: Vec::new(),
        extents: extents.clone(),
    };
    let ps_options = hlsl::glsl::Options {
        targets: vec![target],
        extents,
    };
    let read = |program, names, options| {
        hlsl::glsl(program, names, hlsl::Reading::Plain, options)
            .lines
            .join("\n")
    };

    let mut targets: Vec<String> = ps_names
        .outputs
        .iter()
        .map(|(register, entry)| (*register, entry.name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect();
    if targets.is_empty() {
        targets.push("SV_Target".to_owned());
    }

    let mut attributes = Vec::new();
    for (register, entry) in &vs_names.inputs {
        if let Some((name, offset)) = FIELDS
            .iter()
            .find(|(name, _)| entry.name.eq_ignore_ascii_case(name))
        {
            attributes.push((*name, *register, *offset));
        }
    }

    let held = layouts(&bytes[ps_at..]);
    let mut buffers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for names in [&vs_names, &ps_names] {
        for buffer in names.constants.values() {
            if !seen.insert(buffer.name.clone()) {
                continue;
            }
            buffers.push(Buffer {
                registers: 1,
                members: held.get(&buffer.name).cloned().unwrap_or_default(),
                name: buffer.name.clone(),
            });
        }
    }
    let mut textures = Vec::new();
    for names in [(&vertex, &vs_names), (&fragment, &ps_names)] {
        for (_, _, name) in hlsl::glsl::textures(names.0, names.1) {
            let value = match name.to_ascii_lowercase() {
                held if held.contains("normal") => FLAT_NORMAL,
                held if held.contains("color") || held.contains("diffuse") => [1.0; 4],
                _ => MID_GREY,
            };
            textures.push((name, value));
        }
    }
    textures.sort_by(|left, right| left.0.cmp(&right.0));
    textures.dedup_by(|left, right| left.0 == right.0);

    Ok(Built {
        vertex: read(&vertex, &vs_names, &vs_options),
        fragment: read(&fragment, &ps_names, &ps_options),
        buffers,
        textures,
        attributes,
        targets,
    })
}

/// The sphere the shaders draw, since a package names no geometry of its own.
fn mesh() -> (Vec<Vertex>, Vec<u16>) {
    let mut vertices = Vec::with_capacity((RINGS + 1) * (SEGMENTS + 1));
    for ring in 0..=RINGS {
        let phi = std::f32::consts::PI * ring as f32 / RINGS as f32;
        for segment in 0..=SEGMENTS {
            let theta = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
            let normal = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            let tangent = Vec3::new(-theta.sin(), 0.0, theta.cos());
            vertices.push(Vertex {
                position: [normal.x, normal.y, normal.z, 1.0],
                normal: [normal.x, normal.y, normal.z, 0.0],
                tangent: [tangent.x, tangent.y, tangent.z, 1.0],
                uv: [
                    segment as f32 / SEGMENTS as f32,
                    ring as f32 / RINGS as f32,
                    0.0,
                    0.0,
                ],
                color: [1.0; 4],
            });
        }
    }
    let mut indices = Vec::with_capacity(RINGS * SEGMENTS * 6);
    let stride = SEGMENTS + 1;
    for ring in 0..RINGS {
        for segment in 0..SEGMENTS {
            let at = (ring * stride + segment) as u16;
            let below = at + stride as u16;
            indices.extend([at, below, at + 1, at + 1, below, below + 1]);
        }
    }
    (vertices, indices)
}

/// The bytes one constant buffer holds, filled by field name off the reflection.
fn fill(buffer: &Buffer, defaults: &[f32], view: Mat4, projection: Mat4) -> Vec<u8> {
    let span = buffer
        .members
        .iter()
        .map(|member| member.offset + member.size)
        .max()
        .unwrap_or(0)
        .max(buffer.registers * 16)
        .max(16);
    let mut out = vec![0u8; span.div_ceil(16) as usize * 16];

    if buffer.name == "g_MaterialParameter" {
        for (at, value) in defaults.iter().enumerate() {
            let offset = at * 4;
            if offset + 4 <= out.len() {
                out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        return out;
    }

    // A matrix reads as its rows, since a register of the buffer is a row and the machine takes a
    // dot product against one.
    let rows = |matrix: Mat4, count: usize| -> Vec<f32> {
        let held = matrix.transpose().to_cols_array();
        held[..count * 4].to_vec()
    };
    let inverse_view = view.inverse();
    let view_projection = projection * view;
    let mut put = |name: &str, values: Vec<f32>| {
        let Some(member) = buffer.members.iter().find(|held| held.name == name) else {
            return;
        };
        for (at, value) in values.iter().enumerate() {
            let offset = member.offset as usize + at * 4;
            if offset + 4 <= out.len() {
                out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
    };
    for name in ["m_ViewMatrix", "m_ViewMatrixPrev"] {
        put(name, rows(view, 3));
    }
    for name in [
        "m_InverseViewMatrix",
        "m_InverseViewMatrixPrev",
        "m_MainViewToWorldMatrix",
    ] {
        put(name, rows(inverse_view, 3));
    }
    for name in [
        "m_ViewProjectionMatrix",
        "m_ViewProjectionMatrixPrev",
        "m_MainViewToProjectionMatrix",
    ] {
        put(name, rows(view_projection, 4));
    }
    for name in [
        "m_InverseViewProjectionMatrix",
        "m_InverseViewProjectionMatrixPrev",
    ] {
        put(name, rows(view_projection.inverse(), 4));
    }
    for name in ["m_ProjectionMatrix", "m_ProjectionMatrixPrev"] {
        put(name, rows(projection, 4));
    }
    for name in ["m_InverseProjectionMatrix", "m_InverseProjectionMatrixPrev"] {
        put(name, rows(projection.inverse(), 4));
    }
    for name in ["m_ProjToProjPrevMatrix", "m_ViewToViewPrevMatrix"] {
        put(name, rows(Mat4::IDENTITY, 4));
    }
    // An instance's transform takes an object into view space rather than into the world: a vertex
    // shader multiplies by it and then by the projection alone, with nothing between the two.
    put("m_TransformMatrix", rows(view, 3));
    put("m_SkyVisibility", vec![1.0]);
    put("m_DitherAlpha", vec![1.0]);
    put("m_MulColor", vec![1.0, 1.0, 1.0, 1.0]);
    put("m_Param", vec![1.0, 1.0, 1.0, 1.0]);
    out
}

/// What one frame asks the card for.
struct Frame {
    built: Arc<Built>,
    defaults: Vec<f32>,
    view: Mat4,
    projection: Mat4,
    rect: egui::Rect,
    pixels: f32,
}

/// The card's side of the draw, held between frames so a program is built once.
#[derive(Default)]
struct Gpu {
    program: Option<glow::Program>,
    layout: Option<glow::VertexArray>,
    vertices: Option<glow::Buffer>,
    indices: Option<glow::Buffer>,
    blocks: Vec<glow::Buffer>,
    stand_ins: Vec<glow::Texture>,
    /// The source the program on the card was built from, so a change rebuilds it.
    source: Option<String>,
    failure: Option<String>,
    count: i32,
    /// What the context allows, which is what decides whether the whole G-buffer could be drawn.
    draw_buffers: i32,
}

impl Drop for Gpu {
    fn drop(&mut self) {
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.program.take().map(Dead::Program));
        dead.extend(self.layout.take().map(Dead::Layout));
        dead.extend(self.vertices.take().map(Dead::Buffer));
        dead.extend(self.indices.take().map(Dead::Buffer));
        dead.extend(self.blocks.drain(..).map(Dead::Buffer));
        dead.extend(self.stand_ins.drain(..).map(Dead::Texture));
    }
}

impl Gpu {
    fn upload(&mut self, gl: &glow::Context, frame: &Frame) -> Result<(), String> {
        let source = format!("{}\n{}", frame.built.vertex, frame.built.fragment);
        if self.source.as_deref() == Some(source.as_str()) {
            return Ok(());
        }
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.program.take().map(Dead::Program));
        dead.extend(self.stand_ins.drain(..).map(Dead::Texture));
        dead.extend(self.blocks.drain(..).map(Dead::Buffer));
        drop(dead);
        self.source = Some(source);

        unsafe {
            let program = gl.create_program().map_err(|why| why.to_string())?;
            let mut shaders = Vec::new();
            for (kind, text) in [
                (glow::VERTEX_SHADER, &frame.built.vertex),
                (glow::FRAGMENT_SHADER, &frame.built.fragment),
            ] {
                let shader = gl.create_shader(kind).map_err(|why| why.to_string())?;
                gl.shader_source(shader, text);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            self.program = Some(program);

            if self.layout.is_none() {
                let (vertices, indices) = mesh();
                self.count = indices.len() as i32;
                let layout = gl.create_vertex_array().map_err(|why| why.to_string())?;
                let held = gl.create_buffer().map_err(|why| why.to_string())?;
                let elements = gl.create_buffer().map_err(|why| why.to_string())?;
                gl.bind_vertex_array(Some(layout));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(held));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    bytemuck::cast_slice(&vertices),
                    glow::STATIC_DRAW,
                );
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(elements));
                gl.buffer_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    bytemuck::cast_slice(&indices),
                    glow::STATIC_DRAW,
                );
                gl.bind_vertex_array(None);
                self.layout = Some(layout);
                self.vertices = Some(held);
                self.indices = Some(elements);
            }

            // The mesh supplies the same five fields whatever the shader asks for, so the layout is
            // rebuilt against each shader's own signature.
            gl.bind_vertex_array(self.layout);
            gl.bind_buffer(glow::ARRAY_BUFFER, self.vertices);
            for location in 0..16 {
                gl.disable_vertex_attrib_array(location);
            }
            for (_, location, offset) in &frame.built.attributes {
                gl.enable_vertex_attrib_array(*location);
                gl.vertex_attrib_pointer_f32(
                    *location,
                    4,
                    glow::FLOAT,
                    false,
                    size_of::<Vertex>() as i32,
                    *offset,
                );
            }
            gl.bind_vertex_array(None);

            for (name, value) in &frame.built.textures {
                let texture = gl.create_texture().map_err(|why| why.to_string())?;
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                let pixels: Vec<u8> = value
                    .iter()
                    .map(|held| (held.clamp(0.0, 1.0) * 255.0) as u8)
                    .collect();
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA as i32,
                    1,
                    1,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&pixels)),
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                let _ = name;
                self.stand_ins.push(texture);
            }
        }
        Ok(())
    }

    fn draw(&mut self, gl: &glow::Context, frame: &Frame) {
        bury(gl);
        if self.draw_buffers == 0 {
            self.draw_buffers = unsafe { gl.get_parameter_i32(glow::MAX_DRAW_BUFFERS) };
        }
        if let Err(why) = self.upload(gl, frame) {
            self.failure = Some(why);
            self.source = None;
            return;
        }
        self.failure = None;
        let Some(program) = self.program else {
            return;
        };

        unsafe {
            gl.use_program(Some(program));
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);

            for (at, buffer) in frame.built.buffers.iter().enumerate() {
                let Some(block) =
                    gl.get_uniform_block_index(program, &format!("{}_b", buffer.name))
                else {
                    continue;
                };
                let size = gl.get_active_uniform_block_parameter_i32(
                    program,
                    block,
                    glow::UNIFORM_BLOCK_DATA_SIZE,
                ) as usize;
                let mut data = fill(buffer, &frame.defaults, frame.view, frame.projection);
                data.resize(size.max(16), 0);
                let held = match self.blocks.get(at) {
                    Some(held) => *held,
                    None => {
                        let Ok(held) = gl.create_buffer() else { return };
                        self.blocks.push(held);
                        held
                    }
                };
                gl.bind_buffer(glow::UNIFORM_BUFFER, Some(held));
                gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, &data, glow::DYNAMIC_DRAW);
                gl.bind_buffer_base(glow::UNIFORM_BUFFER, at as u32, Some(held));
                gl.uniform_block_binding(program, block, at as u32);
            }

            for (unit, ((name, _), texture)) in
                frame.built.textures.iter().zip(&self.stand_ins).enumerate()
            {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
                if let Some(location) = gl.get_uniform_location(program, name) {
                    gl.uniform_1_i32(Some(&location), unit as i32);
                }
                if let Some(location) = gl.get_uniform_location(program, &format!("{name}_levels"))
                {
                    gl.uniform_1_i32(Some(&location), 1);
                }
            }
            if let Some(location) = gl.get_uniform_location(program, "dx_Viewport") {
                gl.uniform_2_f32(
                    Some(&location),
                    frame.rect.width() * frame.pixels,
                    frame.rect.height() * frame.pixels,
                );
            }

            gl.bind_vertex_array(self.layout);
            gl.draw_elements(glow::TRIANGLES, self.count, glow::UNSIGNED_SHORT, 0);
            gl.bind_vertex_array(None);
            gl.disable(glow::DEPTH_TEST);
        }
    }
}

/// The camera, which is this viewer's rather than the game's. D3D looks down positive z and keeps
/// clip depth in nought to one; the translated vertex shader moves that to GL's range.
fn camera(rect: egui::Rect, yaw: f32, pitch: f32) -> (Mat4, Mat4) {
    let eye = Vec3::new(
        DISTANCE * pitch.cos() * yaw.sin(),
        DISTANCE * pitch.sin(),
        DISTANCE * pitch.cos() * yaw.cos(),
    );
    let view = Mat4::look_at_lh(eye, Vec3::ZERO, Vec3::Y);
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    (view, Mat4::perspective_lh(FOV, aspect, 0.05, 100.0))
}

pub fn ui(ui: &mut egui::Ui, package: &Rendered, bytes: &[u8]) {
    let state = package.state.with("render");
    let mut target = ui
        .data(|data| data.get_temp::<usize>(state.with("target")))
        .unwrap_or(0);
    let mut orbit = ui
        .data(|data| data.get_temp::<(f32, f32)>(state.with("orbit")))
        .unwrap_or((0.6, 0.4));

    // Parsing again costs a walk of the tables, which is nothing beside translating two shaders, and
    // it saves `Rendered` holding a borrow of the file.
    let held = ui.data(|data| data.get_temp::<(usize, Result<Arc<Built>, String>)>(state));
    let built = match held {
        Some((was, held)) if was == target => held,
        _ => {
            let fresh = ShaderPackage::parse(bytes)
                .map_err(|why| why.to_string())
                .and_then(|package| build(&package, bytes, target as u32).map(Arc::new));
            ui.data_mut(|data| data.insert_temp(state, (target, fresh.clone())));
            fresh
        }
    };

    let gpu = ui.data_mut(|data| {
        data.get_temp_mut_or_default::<Arc<Mutex<Gpu>>>(state.with("gpu"))
            .clone()
    });
    let limit = gpu.lock().unwrap().draw_buffers;

    let built = match built {
        Ok(built) => built,
        Err(why) => {
            ui.label(RichText::new(why).weak());
            return;
        }
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new("Target").weak().small());
        for (at, name) in built.targets.iter().enumerate() {
            if ui.selectable_label(at == target, name).clicked() {
                target = at;
            }
        }
        if limit > 0 {
            ui.label(
                RichText::new(format!(
                    "{limit} draw buffers, {} targets, one at a time",
                    built.targets.len()
                ))
                .weak()
                .small(),
            );
        }
    });
    ui.data_mut(|data| data.insert_temp(state.with("target"), target));

    if let Some(why) = gpu.lock().unwrap().failure.clone() {
        ui.label(RichText::new(why).weak());
    }

    let space = ui.available_size().max(vec2(64.0, 64.0));
    let (rect, response) = ui.allocate_exact_size(space, Sense::drag());
    if response.dragged() {
        let held = response.drag_delta();
        orbit.0 -= held.x * 0.01;
        orbit.1 = (orbit.1 + held.y * 0.01).clamp(-1.5, 1.5);
    }
    ui.data_mut(|data| data.insert_temp(state.with("orbit"), orbit));

    let (view, projection) = camera(rect, orbit.0, orbit.1);
    let frame = Frame {
        built,
        defaults: package.defaults.clone(),
        view,
        projection,
        rect,
        pixels: ui.ctx().pixels_per_point(),
    };
    // The context is taken from the painter rather than captured: `glow::Context` is neither `Send`
    // nor `Sync` on wasm, and a callback has to be both.
    ui.painter().add(egui::PaintCallback {
        rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
            gpu.lock().unwrap().draw(painter.gl(), &frame);
        })),
    });
}

#[cfg(test)]
mod test {
    use super::selector;

    /// Penumbra's own multiplier, applied positionally. A node's id is the same polynomial over the
    /// four groups' own selectors.
    #[test]
    fn the_selector_is_a_polynomial_in_thirty_one() {
        assert_eq!(selector(&[]), 0);
        assert_eq!(selector(&[7]), 7);
        assert_eq!(selector(&[1, 1]), 32);
        assert_eq!(selector(&[0, 0, 1]), 961);
        // Wrapping, so a high key does not overflow the walk.
        assert_eq!(selector(&[u32::MAX, 2]), u32::MAX.wrapping_add(62));
    }
}
