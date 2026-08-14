//! `.mdl` models, drawn.
//!
//! Geometry comes off the file when it is decoded; the materials it names are fetched afterwards and
//! land on meshes already on screen, so a model shows as untextured geometry first and dresses
//! itself as its textures arrive.
//!
//! The shading approximates the game's rather than reproducing it: a color table row is picked the
//! way the game picks one and drives a diffuse color, a specular color and a specular exponent, the
//! mask map scales all three, and everything is lit by three lights that follow the camera instead
//! of by the scene's. Skinning, dyes and decals are all absent, so a character stands in bind pose.
//!
//! Shape keys are applied by rewriting the indices they name, which is what the file states rather
//! than a blend, so a shape is either on or off.

pub(super) mod deferred;
mod deform;
pub(super) mod gpu;
mod grid;
pub(super) mod material;
pub(super) mod program;
mod skin;

pub use deform::{Deform, Deformers};
pub use program::Customize;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use egui::{Color32, RichText, ScrollArea, Sense, TextureHandle, TextureOptions};
use glam::{Mat3, Mat4, Vec3};
use ironworks::file::{
    File,
    imc::ImageChange,
    mdl::{Lod, MeshKind, ModelContainer, VertexAttributeKind, VertexFormat, VertexValues},
    spm::ShaderParameters,
};
use std::io::Cursor;

use super::{Preview, facts, link, placed, section};
use crate::assets::Bytes;
use crate::backend::Backend;
use crate::data::DecodedTexture;
use crate::utils::TrackedPromise;

use material::{Material, Role};

/// What a model's textures may be decoded to, as the longest edge of the mipmap taken. The last
/// takes the file's own mip nought, which is what the game itself draws.
const DETAIL: [(Option<u16>, &str); 4] = [
    (Some(512), "512"),
    (Some(1024), "1024"),
    (Some(2048), "2048"),
    (None, "Authored"),
];

/// Decoded texture bytes one model may hold. Past it the rest of its surfaces draw untextured.
const TEXTURE_BUDGET: usize = 256 << 20;

/// The attributes an imc entry's mask reaches, which the format gives ten bits.
const IMC_ATTRIBUTES: u32 = 0x3ff;

/// Vertical field of view.
const FOV: f32 = 40.0_f32.to_radians();

/// How much of the model's radius the initial framing leaves as margin.
const MARGIN: f32 = 1.25;

/// The scene key deciding whether a shader skins, and the value asking it to. Nothing in a file
/// says it; a mesh carrying bone indices is what the engine would set it from.
const TRANSFORM_VIEW: u32 = 0xa5a1_910d;
const TRANSFORM_VIEW_SKIN: u32 = 0x9c14_c8e9;

/// The scene key deciding whether a background shader reads the normal map at all. A package
/// defaults it to off, and the variant that answer selects samples no normal map, so the frame it
/// writes is the geometry's own.
const GET_NORMAL_MAP: u32 = 0xcbdf_d5ec;
const GET_NORMAL_MAP_ON: u32 = 0xd999_4ef1;

/// The scene key deciding whether a character shader clips against its own alpha threshold. A
/// package defaults it to off, and the variant that answer selects carries no clip at all, so a
/// material's cutout leaves the geometry it was authored over standing.
const APPLY_ALPHA_CLIP: u32 = 0xdcfc_844e;
const APPLY_ALPHA_CLIP_ON: u32 = 0x59c4_e6db;

/// Where the key light stands, in the model's own space. Anchored rather than carried with the
/// camera: a rig that turns with the eye shades every angle alike, so orbiting reveals no form.
const KEY: Vec3 = Vec3::new(-0.45, 0.78, 0.44);

/// How far the placed light reaches, in radii of the model. A lamp is drawn as the box it covers
/// and cut off at the sphere of its own reach, and both of those show as a hard edge where they
/// cross what is drawn, so the box stands well outside it. The near and far planes have to hold the
/// whole box: a face of it that the planes cut leaves a straight edge that moves with the camera.
const LAMP_SPAN: f32 = 4.0;

/// What the placed light is worth beside the sun. At one it is a second key rather than a fill, and
/// a pale surface under two keys is lit to the top of what the frame holds, with no shading left.
const LAMP_FILL: f32 = 0.3;

/// A vertex as the shader reads it. `#[repr(C)]` with no padding, so a mesh uploads as its own
/// slice.
///
/// Every semantic a drawing package asks for is here whether or not a given shader reads it, so one
/// upload serves both this viewer's own shading and the game's, which bind different subsets of it.
/// Tangents are kept as the file states them, since the game's own shaders do their own unbiasing.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    tangent: [f32; 4],
    bitangent: [f32; 4],
    uv: [f32; 4],
    uv1: [f32; 4],
    color: [u8; 4],
    color1: [u8; 4],
    /// Sixteen bits each, since a skinned shader reads the low byte as the first four influences
    /// and the high byte as the next four.
    weights: [u16; 4],
    bones: [u16; 4],
}

/// Where the camera is looking from.
#[derive(Clone, Copy)]
struct Camera {
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: Vec3,
}

impl Camera {
    fn eye(&self) -> Vec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        self.target + self.distance * Vec3::new(cos_pitch * sin_yaw, sin_pitch, cos_pitch * cos_yaw)
    }
}

/// One part of a mesh, drawn with the rest of it but hideable on its own.
struct Part {
    range: Range<usize>,
    /// A cell rather than a plain bool: the imc variant this defaults from is fetched after the
    /// level is built, and applying it only has to reach the part, not rebuild the level around it.
    shown: Cell<bool>,
    /// What the model's attribute table calls this part, which is the only name it carries. Empty
    /// where the part claims no attribute.
    attributes: String,
    /// The bits behind that name, as the file's own attribute mask states them. Nought where the
    /// part claims no attribute, which is what leaves it shown whatever variant is picked.
    mask: u32,
}

/// One mesh of the model, as far as the browser cares about it.
struct Mesh {
    /// Which of the level's pieces this came out of, since each carries its own `.imc` and so its
    /// own attribute mask.
    piece: usize,
    material: usize,
    vertices: usize,
    triangles: usize,
    /// The runs of indices the file splits the mesh into, and whether each draws. A mesh the file
    /// does not split holds the one run covering all of them.
    parts: Vec<Part>,
    /// The mesh's indices as the file lists them, kept only where the model has a shape key that
    /// could rewrite them, since applying one is a rewrite of these rather than of what is on the
    /// card.
    base: Vec<u16>,
}

/// Which of the level's meshes a shape touches, and for each the indices it replaces.
type Rewrites = Vec<(usize, Vec<(u16, u16)>)>;

/// One shape key, and where it rewrites the geometry.
struct Shape {
    name: String,
    rewrites: Rewrites,
}

/// Shape keys the file names as variants of one thing, which the browser offers as alternatives
/// rather than as switches that stack. A name carrying no variant stands in a group of its own.
struct Group {
    /// The file's own abbreviation, left as it writes it. Empty for a shape standing alone.
    category: String,
    /// Positions in [`Level::shapes`], each with the variant its name ends in.
    variants: Vec<(usize, String)>,
}

/// A texture, from the moment it is asked for to the moment it can be bound.
enum Texture {
    Fetching(TrackedPromise<Result<DecodedTexture>>),
    Ready(TextureHandle),
    /// It would not load, or the model had already spent its budget.
    Absent,
}

/// A material, from the moment it is asked for to the moment it can be drawn with.
enum Slot {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Box<Material>),
    Failed(String),
}

/// A shader package, from the moment a material names it to the moment it can be translated.
enum Package {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Vec<u8>),
    Failed(String),
}

/// One of the game's own texture arrays. Kept once decoded, since a level built later has a context
/// of its own to hand it to and asking for it again would be a fetch the user watches land.
enum Array {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(deferred::Layered),
    Failed,
}

/// One of the game's own shader parameter files. Kept once parsed, since the table every file writes
/// into is built again from all of them each time one more arrives.
enum Parameters {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(ShaderParameters),
    Failed,
}

/// The model's own `.imc`, which states which attribute-gated parts a variant draws. `Absent` covers
/// both a model this could name no such file for and one whose file would not read, and either way
/// means what today means: every part shows, whatever it claims an attribute for.
enum Imc {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(ImageChange),
    Absent,
}

/// One material's translated shaders, and what they were translated for: how many attachments the
/// context allows decides how many of the G-buffer's targets fit in one reading.
struct Translated {
    attachments: usize,
    held: Result<Passes, String>,
}

/// What a material draws with. A semitransparent package declares no G pass at all: it is drawn
/// over the frame the composite resolved rather than into the buffer that frame came from.
#[derive(Default)]
struct Passes {
    /// The buffer pass, one reading per page of its targets.
    buffer: Vec<Arc<program::Program>>,
    depth: Option<Arc<program::Program>>,
    /// What the material resolves itself into the frame with, drawn as its own geometry after the
    /// lighting. A package with no buffer pass has only the semitransparent one.
    resolve: Option<Arc<program::Program>>,
}

/// The color table in the game's own layout: its halfs, the texels a row takes, and the rows.
type Table = Arc<(Vec<u16>, usize, usize)>;

/// One detail level's geometry, and everything the browser says about it.
struct Level {
    identity: Vec<(&'static str, String)>,
    meshes: Vec<Mesh>,
    /// Shape keys reaching this detail level, in the order the file declares them.
    shapes: Vec<Shape>,
    /// The same shapes, gathered by the category their names share.
    groups: Vec<Group>,
    /// Material paths, in the order meshes index them.
    materials: Vec<String>,
    /// Meshes the file lists but whose vertices would not read, with why.
    unreadable: Vec<(usize, String)>,
    /// Framing the model starts at, so the view can be put back.
    home: Camera,
    /// Half the bounding box's diagonal, which the depth range is cut to.
    radius: f32,
    /// Whether any mesh carries bone indices, which is what decides whether the game would draw
    /// this model through its skinning variant.
    skinned: bool,
    /// The bones each mesh's blend indices name, in the order they index them.
    bones: Vec<Vec<String>>,
    /// How many attributes each piece declares. An imc variant's mask means something over only
    /// this many of its bits; the rest are padding the format reserves rather than states.
    attributes: Vec<usize>,
    gpu: Arc<Mutex<gpu::Model>>,
}

/// One file the level was built from. A character is worn out of several, and each carries its own
/// `.imc`, so the variant a part's visibility is read from is the piece's rather than the level's.
struct Piece {
    path: String,
    bytes: Vec<u8>,
    /// The file's own `.imc`, once asked for.
    imc: RefCell<Option<Imc>>,
    /// Which of that imc's variants a part's default visibility is drawn from. Nought is the file's
    /// own default entry.
    variant: Cell<u16>,
    deform: Option<Arc<Deform>>,
    skin: Option<u16>,
}

/// One file to build a model out of, and the imc variant it is worn at. Nought is the file's own
/// default entry, which is what anything inspected on its own is shown at.
pub struct Source {
    pub path: String,
    pub bytes: Vec<u8>,
    pub variant: u16,
    /// What to move the file's vertices by, where it was modelled for a body other than the one
    /// wearing it.
    pub deform: Option<Arc<Deform>>,
    /// The body whose skin to draw it with, where it is a body's own model.
    pub skin: Option<u16>,
}

impl Piece {
    fn new(source: &Source) -> Self {
        Self {
            path: source.path.clone(),
            bytes: source.bytes.clone(),
            imc: RefCell::new(None),
            variant: Cell::new(source.variant),
            deform: source.deform.clone(),
            skin: source.skin,
        }
    }

    /// The picked variant's attribute mask, once the imc has arrived. `None` before it has, or where
    /// it named nothing to read: a part with an attribute then draws exactly as one without.
    fn mask(&self) -> Option<u32> {
        let held = self.imc.borrow();
        let Some(Imc::Ready(image_change)) = held.as_ref() else {
            return None;
        };
        image_change
            .entry(imc_part(&self.path), self.variant.get())
            .map(|entry| u32::from(entry.attribute_mask()))
    }

    /// How many variants past the default the imc carries, once it has arrived.
    fn variants(&self) -> Option<u16> {
        match self.imc.borrow().as_ref() {
            Some(Imc::Ready(image_change)) => Some(image_change.variant_count()),
            _ => None,
        }
    }
}

/// Everything a material owns that outlives the level it was built for.
type Kept = (Option<Slot>, Option<Translated>, Option<Table>);

/// A model, decoded and ready to draw. Everything a detail level owns is rebuilt when one is
/// picked; the camera and the fetched materials and textures are not, so switching neither moves
/// the view nor asks for anything twice.
pub struct Rendered {
    /// The files the level was merged from, in the order its meshes name them.
    pieces: Vec<Piece>,
    lod: Cell<u8>,
    /// Which detail levels the file draws anything at.
    drawn: [bool; 3],
    level: RefCell<Level>,
    /// Shape keys the user has switched on, by name: a detail level built later carries its own
    /// shapes, and the names are what survives the switch.
    shapes: RefCell<BTreeSet<String>>,
    slots: RefCell<Vec<Option<Slot>>>,
    textures: RefCell<BTreeMap<String, Texture>>,
    /// Shader packages the materials name, by path, since several materials share one.
    packages: RefCell<BTreeMap<String, Package>>,
    /// The textures the shaders read that no material names, by resource id.
    arrays: RefCell<BTreeMap<u32, Array>>,
    /// The parameter files the shader type table is filled from, by the record their first profile
    /// lands at.
    parameters: RefCell<BTreeMap<usize, Parameters>>,
    /// The translated shaders, by material.
    translated: RefCell<BTreeMap<usize, Translated>>,
    /// The skeleton the model is skinned to, and the motion it is posed by.
    animation: skin::Animation,
    /// The passes that light the G-buffer, which belong to the frame rather than to a material.
    lighting: RefCell<Option<Arc<gpu::Lighting>>>,
    /// The pass that grades the frame they resolve, and whether the table it reads has landed.
    post: RefCell<Option<Arc<program::Program>>>,
    graded: Cell<bool>,
    /// The pair that smooths the graded frame's edges, and the chain that occludes it. The second
    /// is kept against the quality it was built at, since that decides which file it came from.
    smoothing: RefCell<Option<Arc<gpu::Smoothing>>>,
    occlusion: RefCell<Option<(usize, Arc<gpu::Occlusion>)>>,
    /// What those passes are run with, and whether the settings row is open.
    look: Cell<program::Look>,
    settings: Cell<bool>,
    /// The color table in the game's own layout, by material.
    tables: RefCell<BTreeMap<usize, Table>>,
    camera: Cell<Camera>,
    /// Which of the two viewers this is, which is what decides how much of the model it takes apart.
    chrome: Cell<Chrome>,
    /// The colours the character was made with, and the attributes it does not wear. A face
    /// declares one part per facial feature and no `.imc` to choose between them, so left to the
    /// variant alone it draws all seven at once over each other.
    customize: Cell<program::Customize>,
    hidden: RefCell<BTreeSet<String>>,
    /// How tall the character was built, as a scale on everything it is drawn from.
    stature: Cell<f32>,
    /// Whether the rig it is posed on is drawn over it, and the boxes that is drawn as.
    skeleton: Cell<bool>,
    overlay: Arc<Mutex<placed::Placements>>,
    /// Whether a floor is ruled at the origin under it.
    grid: Cell<bool>,
    /// Decoded texture bytes handed to egui so far.
    resident: Cell<usize>,
    debug: Cell<gpu::Debug>,
    /// Whether to draw with the game's own shaders rather than with this viewer's approximation.
    shaded: Cell<bool>,
    /// Which G-buffer channel the game's own shaders put on screen, starting at the frame their
    /// lighting resolves rather than at a channel of the buffer it is resolved from.
    target: Cell<usize>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let model = compose(&[Source {
        path: path.to_owned(),
        bytes: bytes.to_vec(),
        variant: 0,
        deform: None,
        skin: None,
    }])?;
    model.chrome.set(Chrome::Asset);
    model.shaded.set(false);
    model.animation.rest();
    Ok(Preview::Model(Box::new(model)))
}

/// Builds one model out of several files, which is how a character is worn. The first is what the
/// rest hang off: its path is what names the skeleton they are all posed on. A character is drawn
/// the way the game draws it, standing in its idle rather than in the pose its files hold.
pub fn compose(parts: &[Source]) -> Result<Rendered> {
    parts.first().context("a model of no files")?;
    let pieces: Vec<_> = parts.iter().map(Piece::new).collect();
    let drawn = drawn_levels(&pieces)?;
    let level = level_of(&pieces, 0)?;
    let camera = level.home;
    Ok(Rendered {
        pieces,
        lod: Cell::new(0),
        drawn,
        slots: RefCell::new((0..level.materials.len()).map(|_| None).collect()),
        shapes: Default::default(),
        level: RefCell::new(level),
        textures: Default::default(),
        packages: Default::default(),
        arrays: Default::default(),
        parameters: Default::default(),
        translated: Default::default(),
        animation: skin::Animation::new(parts.iter().map(|part| part.path.as_str())),
        lighting: Default::default(),
        post: Default::default(),
        graded: Cell::new(false),
        smoothing: Default::default(),
        occlusion: Default::default(),
        look: Cell::new(program::Look::default()),
        settings: Cell::new(false),
        tables: Default::default(),
        camera: Cell::new(camera),
        chrome: Cell::new(Chrome::Character),
        customize: Cell::new(program::Customize::default()),
        hidden: Default::default(),
        stature: Cell::new(1.0),
        skeleton: Cell::new(false),
        overlay: placed::Placements::new(Vec::new()),
        grid: Cell::new(true),
        resident: Cell::new(0),
        debug: Cell::new(gpu::Debug::None),
        shaded: Cell::new(true),
        target: Cell::new(gpu::LIT),
    })
}

fn containers(pieces: &[Piece]) -> Result<Vec<ModelContainer>> {
    pieces
        .iter()
        .map(|piece| Ok(ModelContainer::read(Cursor::new(piece.bytes.clone()))?))
        .collect()
}

fn level_of(pieces: &[Piece], lod: u8) -> Result<Level> {
    let containers = containers(pieces)?;
    let sources: Vec<_> = pieces
        .iter()
        .map(|piece| Worn {
            path: piece.path.as_str(),
            variant: piece.variant.get(),
            deform: piece.deform.as_deref(),
            skin: piece.skin,
        })
        .zip(&containers)
        .collect();
    read_level(&sources, lod)
}

/// Which detail levels the pieces draw anything at.
fn drawn_levels(pieces: &[Piece]) -> Result<[bool; 3]> {
    let containers = containers(pieces)?;
    Ok(std::array::from_fn(|lod| {
        containers.iter().any(|container| {
            container
                .model(detail(lod as u8))
                .meshes()
                .iter()
                .any(|mesh| mesh.kinds().contains(&MeshKind::Standard))
        })
    }))
}

pub(super) fn detail(lod: u8) -> Lod {
    match lod {
        0 => Lod::High,
        1 => Lod::Medium,
        _ => Lod::Low,
    }
}

/// What a mesh a drawing pass leaves out is for. Only `Standard` is drawn here: the rest are the
/// engine's own passes, which nothing in this graph runs.
fn kind_name(kind: MeshKind) -> &'static str {
    match kind {
        MeshKind::Water => "water",
        MeshKind::Shadow => "shadow",
        MeshKind::Terrain => "terrain shadow",
        MeshKind::VerticalFog => "vertical fog",
        MeshKind::LightShaft => "light shaft",
        MeshKind::Glass => "glass",
        MeshKind::MaterialChange => "material change",
        MeshKind::CrestChange => "crest change",
        MeshKind::Standard => "standard",
    }
}

/// The `.imc` this model's part draws with, derived from the model's own path rather than named
/// anywhere in the file: strip the `model/<name>.mdl` tail and the directory left names it.
fn imc_path(path: &str) -> Option<String> {
    let base = &path[..path.rfind("/model/")?];
    let part = base.rsplit('/').next()?;
    Some(format!("{base}/{part}.imc"))
}

/// Which of the imc's five parts this model's own slot reads: head or ears 0, body or neck 1,
/// hands or wrists 2, legs or right ring 3, feet or left ring 4, matching `imc.rs`'s own doc. A
/// monster or weapon has one part and no such suffix, so it falls back to 0, which is already
/// right for it.
fn imc_part(path: &str) -> u8 {
    let stem = path.rsplit('/').next().unwrap_or(path);
    let slot = stem
        .strip_suffix(".mdl")
        .unwrap_or(stem)
        .rsplit('_')
        .next()
        .unwrap_or("");
    match slot {
        "met" | "ear" => 0,
        "top" | "nek" => 1,
        "glv" | "wrs" => 2,
        "dwn" | "rir" => 3,
        "sho" | "ril" => 4,
        _ => 0,
    }
}

/// Whether an imc's variants pick between mutually exclusive geometry, which is what lets a first
/// look default past entry 0's own all-on catalog: variant 1's mask is set, at least one other
/// variant's is too, and no two share a bit. Masks are restricted to the model's own declared
/// attributes, since the format reserves ten bits regardless of how many exist; a lone alternative
/// is a toggle rather than a choice and does not count, which is what keeps this off ordinary
/// equipment.
fn exclusive_variants(image_change: &ImageChange, part: u8, declared: usize) -> bool {
    let cover: u32 = match declared {
        0 => return false,
        1..32 => (1u32 << declared) - 1,
        _ => u32::MAX,
    };
    let first = image_change
        .entry(part, 1)
        .map_or(0, |entry| u32::from(entry.attribute_mask()) & cover);
    if first == 0 {
        return false;
    }
    let mut seen = first;
    let mut count = 1;
    for variant in 2..=image_change.variant_count() {
        let Some(mask) = image_change
            .entry(part, variant)
            .map(|entry| u32::from(entry.attribute_mask()) & cover)
        else {
            continue;
        };
        if mask == 0 {
            continue;
        }
        if seen & mask != 0 {
            return false;
        }
        seen |= mask;
        count += 1;
    }
    count >= 2
}

/// What a piece contributes to a level beyond the file it was decoded from.
struct Worn<'a> {
    path: &'a str,
    variant: u16,
    deform: Option<&'a Deform>,
    skin: Option<u16>,
}

fn read_level(sources: &[(Worn<'_>, &ModelContainer)], lod: u8) -> Result<Level> {
    let mut names: Vec<String> = Vec::new();
    let mut meshes = Vec::new();
    let mut unreadable = Vec::new();
    let mut pending = gpu::Pending::default();
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    let mut bones: Vec<Vec<String>> = Vec::new();
    let mut shapes: Vec<Shape> = Vec::new();
    let mut declares: Vec<usize> = Vec::new();
    let mut skipped: Vec<MeshKind> = Vec::new();
    let mut skinned = false;

    for (piece, (worn, container)) in sources.iter().enumerate() {
        let model = container.model(detail(lod));

        let attributes = model.attribute_names().unwrap_or_default();
        let bone_names = model.bone_names().unwrap_or_default();
        let declared = model.shapes();
        let mut rewrites: Vec<Rewrites> = declared.iter().map(|_| Vec::new()).collect();
        declares.push(attributes.len());

        for (index, mesh) in model.meshes().into_iter().enumerate() {
            if !mesh.kinds().contains(&MeshKind::Standard) {
                for kind in mesh.kinds() {
                    if !skipped.contains(kind) {
                        skipped.push(*kind);
                    }
                }
                continue;
            }
            let built = match (mesh.attributes(), mesh.indices()) {
                (Ok(attributes), Ok(indices)) => {
                    skinned |= attributes.iter().any(|attribute| {
                        attribute.kind as u8 == VertexAttributeKind::BlendIndices as u8
                    });
                    build(&attributes, indices)
                }
                (Err(why), _) | (_, Err(why)) => Err(why.to_string()),
            };
            let (mut vertices, indices) = match built {
                Ok(built) => built,
                Err(why) => {
                    unreadable.push((index, why));
                    continue;
                }
            };

            let table: Vec<String> = mesh
                .bone_table()
                .iter()
                .map(|bone| {
                    bone_names
                        .get(usize::from(*bone))
                        .cloned()
                        .unwrap_or_default()
                })
                .collect();
            if let Some(deform) = worn.deform {
                deform.apply(&mut vertices, &table);
            }

            for vertex in &vertices {
                let position = Vec3::from_array(vertex.position);
                low = low.min(position);
                high = high.max(position);
            }

            let name = mesh.material().unwrap_or_default();
            let resolved = material::path(&name, worn.variant, worn.skin).unwrap_or(name);
            let material = names
                .iter()
                .position(|held| *held == resolved)
                .unwrap_or_else(|| {
                    names.push(resolved);
                    names.len() - 1
                });
            let submeshes = mesh.submeshes();
            let parts = match submeshes.is_empty() {
                true => vec![Part {
                    range: 0..indices.len(),
                    shown: Cell::new(true),
                    attributes: String::new(),
                    mask: 0,
                }],
                false => submeshes
                    .iter()
                    .map(|part| Part {
                        range: part.start..part.start + part.count,
                        shown: Cell::new(true),
                        attributes: named(&attributes, part.attributes),
                        mask: part.attributes,
                    })
                    .collect(),
            };
            for (shape, touched) in declared.iter().zip(&mut rewrites) {
                let values = shape.rewrites(&mesh);
                if !values.is_empty() {
                    touched.push((meshes.len(), values));
                }
            }
            bones.push(table);
            meshes.push(Mesh {
                piece,
                material,
                vertices: vertices.len(),
                triangles: indices.len() / 3,
                parts,
                base: match declared.is_empty() {
                    true => Vec::new(),
                    false => indices.clone(),
                },
            });
            pending.meshes.push((vertices, indices));
        }

        shapes.extend(
            declared
                .iter()
                .zip(rewrites)
                .filter(|(_, touched)| !touched.is_empty())
                .map(|(shape, touched)| Shape {
                    name: shape.name().unwrap_or_default(),
                    rewrites: touched,
                }),
        );
    }

    // A model whose every mesh carries a kind nothing here draws still has materials, a tree and a
    // browser worth opening, so the level comes back empty and names what it left out rather than
    // the read failing. A mesh that would not read at all is a different matter.
    if meshes.is_empty()
        && let Some((_, why)) = unreadable.first()
    {
        anyhow::bail!("no mesh of this model could be read: {why}");
    }
    if meshes.is_empty() {
        low = Vec3::NEG_ONE;
        high = Vec3::ONE;
    }

    let center = (low + high) * 0.5;
    let radius = ((high - low).length() * 0.5).max(0.01);
    let home = Camera {
        yaw: 0.0,
        pitch: 0.15,
        distance: radius / (FOV * 0.5).tan() * MARGIN,
        target: center,
    };

    let vertices: usize = meshes.iter().map(|mesh| mesh.vertices).sum();
    let triangles: usize = meshes.iter().map(|mesh| mesh.triangles).sum();
    let mut identity = vec![
        ("Meshes", meshes.len().to_string()),
        ("Vertices", vertices.to_string()),
        ("Triangles", triangles.to_string()),
        ("Materials", names.len().to_string()),
        (
            "Bounds",
            format!(
                "{:.2} x {:.2} x {:.2}",
                high.x - low.x,
                high.y - low.y,
                high.z - low.z
            ),
        ),
        (
            "Buffers",
            Bytes(vertices * size_of::<Vertex>() + triangles * 6).to_string(),
        ),
    ];
    if !skipped.is_empty() {
        identity.push((
            "Not drawn",
            skipped
                .iter()
                .map(|kind| kind_name(*kind))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    log::info!(
        "assets/mdl: {} {} meshes, {vertices} vertices, {} materials, {} unreadable",
        sources
            .iter()
            .map(|(worn, _)| crate::utils::file_name(worn.path))
            .collect::<Vec<_>>()
            .join(" + "),
        meshes.len(),
        names.len(),
        unreadable.len()
    );

    Ok(Level {
        identity,
        groups: group(&shapes),
        meshes,
        shapes,
        materials: names,
        unreadable,
        home,
        radius,
        skinned,
        bones,
        attributes: declares,
        gpu: gpu::Model::new(pending),
    })
}

/// Interleaves the attributes a mesh declares into the one buffer the shader reads. A mesh missing
/// a normal, tangent, UV or color gets a default rather than being dropped.
pub(super) fn build(
    attributes: &[ironworks::file::mdl::VertexAttribute],
    indices: Vec<u16>,
) -> Result<(Vec<Vertex>, Vec<u16>), String> {
    let held = |kind: u8, usage: u8| {
        attributes
            .iter()
            .find(|attribute| attribute.kind as u8 == kind && attribute.usage_index == usage)
    };
    let positions = held(VertexAttributeKind::Position as u8, 0);
    let normals = held(VertexAttributeKind::Normal as u8, 0);
    let tangents = held(VertexAttributeKind::Tangent1 as u8, 0);
    let bitangents = held(VertexAttributeKind::Tangent2 as u8, 0);
    let uvs = held(VertexAttributeKind::Uv as u8, 0);
    let uvs1 = held(VertexAttributeKind::Uv as u8, 1);
    let colors = held(VertexAttributeKind::Color as u8, 0);
    let colors1 = held(VertexAttributeKind::Color as u8, 1);
    let weights = held(VertexAttributeKind::BlendWeights as u8, 0);
    let bones = held(VertexAttributeKind::BlendIndices as u8, 0);

    let Some(positions) = positions.map(|held| &held.values) else {
        return Err("mesh declares no vertex positions".into());
    };
    let count = match positions {
        VertexValues::Vector3(values) => values.len(),
        VertexValues::Vector4(values) => values.len(),
        _ => return Err("vertex positions are not a vector".into()),
    };
    if let Some(index) = indices.iter().find(|index| usize::from(**index) >= count) {
        return Err(format!(
            "index {index} names none of the mesh's {count} vertices"
        ));
    }

    let (normals, uvs, uvs1) = (values(normals), values(uvs), values(uvs1));
    let (colors, colors1) = (values(colors), values(colors1));
    let (weights, bones) = (values(weights), values(bones));
    // A byte tangent arrives scaled to nought and one, which is the convention the game's own
    // shaders unbias from, so a half or float one is put back into it rather than the other way.
    let frame = |held: Option<&ironworks::file::mdl::VertexAttribute>, at| {
        let held = held?;
        let value = xyzw(&held.values, at)?;
        Some(match held.format {
            VertexFormat::ByteFloat4 => value,
            _ => value.map(|channel| channel * 0.5 + 0.5),
        })
    };

    let vertices = (0..count)
        .map(|at| Vertex {
            position: xyz(positions, at).unwrap_or_default(),
            normal: normals
                .and_then(|held| xyz(held, at))
                .unwrap_or([0.0, 1.0, 0.0]),
            // A mesh with no frame gets a flat one, which unbiases to the surface normal rather
            // than to a basis nothing measured.
            tangent: frame(tangents, at).unwrap_or([0.5, 0.5, 1.0, 1.0]),
            bitangent: frame(bitangents, at).unwrap_or([0.5, 0.5, 1.0, 1.0]),
            uv: uvs.and_then(|held| uv(held, at)).unwrap_or_default(),
            uv1: uvs1.and_then(|held| uv(held, at)).unwrap_or_default(),
            color: colors.and_then(|held| bytes(held, at)).unwrap_or([255; 4]),
            color1: colors1.and_then(|held| bytes(held, at)).unwrap_or([255; 4]),
            weights: influences(weights, at, [255, 0, 0, 0]),
            bones: influences(bones, at, [0; 4]),
        })
        .collect();
    Ok((vertices, indices))
}

fn values(held: Option<&ironworks::file::mdl::VertexAttribute>) -> Option<&VertexValues> {
    Some(&held?.values)
}

fn xyz(values: &VertexValues, at: usize) -> Option<[f32; 3]> {
    match values {
        VertexValues::Vector3(held) => held.get(at).copied(),
        VertexValues::Vector4(held) => held.get(at).map(|value| [value[0], value[1], value[2]]),
        _ => None,
    }
}

fn xyzw(values: &VertexValues, at: usize) -> Option<[f32; 4]> {
    match values {
        VertexValues::Vector4(held) => held.get(at).copied(),
        _ => None,
    }
}

/// A half4 UV element carries two sets packed as `xy` and `zw`, so the whole element goes across
/// rather than only the first two components.
fn uv(values: &VertexValues, at: usize) -> Option<[f32; 4]> {
    match values {
        VertexValues::Vector2(held) => held.get(at).map(|value| [value[0], value[1], 0.0, 0.0]),
        VertexValues::Vector3(held) => held
            .get(at)
            .map(|value| [value[0], value[1], value[2], 0.0]),
        VertexValues::Vector4(held) => held.get(at).copied(),
        _ => None,
    }
}

/// One vertex's bone influences, in the sixteen bits a skinned shader reads each as: the low byte
/// of a pair is one of the first four influences and the high byte one of the second four.
fn influences(values: Option<&VertexValues>, at: usize, missing: [u16; 4]) -> [u16; 4] {
    match values {
        Some(VertexValues::Bytes8(held)) => match held.get(at) {
            Some(held) => {
                std::array::from_fn(|lane| u16::from_le_bytes([held[lane * 2], held[lane * 2 + 1]]))
            }
            None => missing,
        },
        held => held
            .and_then(|held| bytes(held, at))
            .map_or(missing, |held| held.map(u16::from)),
    }
}

/// Four bytes of an attribute the shader reads as bytes. An eight-byte element carries two sets
/// interleaved, the low half first, so its own four are every other one.
fn bytes(values: &VertexValues, at: usize) -> Option<[u8; 4]> {
    match values {
        VertexValues::Vector4(held) => held
            .get(at)
            .map(|value| value.map(|channel| (channel.clamp(0.0, 1.0) * 255.0) as u8)),
        VertexValues::Bytes8(held) => held
            .get(at)
            .map(|value| [value[0], value[2], value[4], value[6]]),
        VertexValues::Uint(held) => held.get(at).map(|value| value.to_le_bytes()),
        _ => None,
    }
}

/// Shapes gathered by category, in the order the file declares them. A name is read as
/// `shp_<category>_<variant>`; most carry no variant, and each of those stands alone.
fn group(shapes: &[Shape]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (at, shape) in shapes.iter().enumerate() {
        let (category, variant) = match shape
            .name
            .strip_prefix("shp_")
            .and_then(|rest| rest.rsplit_once('_'))
        {
            Some((category, variant)) => (category.to_owned(), variant.to_owned()),
            None => (String::new(), shape.name.clone()),
        };
        match groups
            .iter_mut()
            .find(|group| !group.category.is_empty() && group.category == category)
        {
            Some(group) => group.variants.push((at, variant)),
            None => groups.push(Group {
                category,
                variants: vec![(at, variant)],
            }),
        }
    }
    groups
}

/// What the model's attribute table calls the bits a part sets. The mask is 32 bits wide however
/// many names the table holds.
fn named(attributes: &[String], mask: u32) -> String {
    attributes
        .iter()
        .take(32)
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The table the shading passes index, from the parameter files that have arrived. Nothing until one
/// has, since a table of nought is what the frame already stands in with.
fn types(parameters: &BTreeMap<usize, Parameters>) -> Option<Vec<u32>> {
    let held = parameters
        .iter()
        .filter_map(|(base, held)| match held {
            Parameters::Ready(file) => Some((*base, file)),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!held.is_empty()).then(|| program::shader_types(&held))
}

/// One of the game's own textures as the card takes it. Mip nought alone: nothing tells a translated
/// shader how many levels a texture has, and the graph answers that with one.
pub(super) fn layered(bytes: &[u8], path: &str, filter: u32) -> Result<deferred::Layered> {
    let texture = ironworks::file::tex::Texture::read(Cursor::new(bytes.to_vec()))?;
    let image = crate::utils::tex_loader::decode_stack(&texture, 0, path)?;
    let (width, height) = texture.mip_size(0);
    Ok(deferred::Layered {
        size: (width.into(), height.into()),
        layers: texture.layers(0).into(),
        pixels: image.into_rgba8().into_raw(),
        filter,
        volumetric: texture.kind() == ironworks::file::tex::TextureKind::D3,
    })
}

/// The parts still showing, as the fewest runs that cover them. A file lists a mesh's parts in
/// index order, so two neighbours that both draw are one call rather than two.
fn shown(parts: &[Part]) -> Vec<Range<i32>> {
    let mut runs: Vec<Range<i32>> = Vec::new();
    for part in parts.iter().filter(|part| part.shown.get()) {
        let run = part.range.start as i32..part.range.end as i32;
        match runs.last_mut() {
            Some(last) if last.end == run.start => last.end = run.end,
            _ => runs.push(run),
        }
    }
    runs
}

/// What the viewer is for: taking a file apart, or standing a character up the way the game does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    Asset,
    Character,
}

/// What the debug row offers, in the order it offers it.
const VIEWS: [(gpu::Debug, &str); 9] = [
    (gpu::Debug::Normals, "Normals"),
    (gpu::Debug::Geometry, "Geometric"),
    (gpu::Debug::Tangents, "Tangents"),
    (gpu::Debug::Bitangents, "Bitangents"),
    (gpu::Debug::Handedness, "Handedness"),
    (gpu::Debug::Uv, "UVs"),
    (gpu::Debug::Color, "Vertex color"),
    (gpu::Debug::Alpha, "Vertex alpha"),
    (gpu::Debug::Meshes, "Meshes"),
];

pub fn ui(ui: &mut egui::Ui, model: &Rendered, backend: &Backend) {
    ui.horizontal_wrapped(|ui| {
        // A character is shown as the game draws it, so the row that takes it apart is not offered:
        // the shaders are already on and there is nothing to switch them to.
        let inspecting = model.chrome.get() == Chrome::Asset;
        let shaded = model.shaded.get();
        if inspecting
            && ui
                .selectable_label(shaded, "Game shaders")
                .on_hover_text("Draw with the package the material names, into its own G-buffer")
                .clicked()
        {
            model.shaded.set(!shaded);
        }
        match shaded {
            true if inspecting => {
                for (at, name) in model.channels() {
                    if ui
                        .selectable_label(model.target.get() == at, name)
                        .clicked()
                    {
                        model.target.set(at);
                    }
                }
            }
            false if inspecting => {
                let debug = model.debug.get();
                for (mode, label) in VIEWS {
                    if ui.selectable_label(debug == mode, label).clicked() {
                        model.debug.set(match debug == mode {
                            true => gpu::Debug::None,
                            false => mode,
                        });
                    }
                }
            }
            _ => {}
        }
        let level = model.level.borrow();
        if level.skinned {
            let skeleton = model.skeleton.get();
            if ui
                .selectable_label(skeleton, "Skeleton")
                .on_hover_text("Draw the rig it is posed on over it")
                .clicked()
            {
                model.skeleton.set(!skeleton);
            }
        }
        let grid = model.grid.get();
        if ui
            .selectable_label(grid, "Grid")
            .on_hover_text("Rule a floor at the origin, at the model's own scale")
            .clicked()
        {
            model.grid.set(!grid);
        }
        if shaded {
            let settings = model.settings.get();
            if ui
                .selectable_label(settings, "Graphics")
                .on_hover_text("What the passes past the composite are run with")
                .clicked()
            {
                model.settings.set(!settings);
            }
        }
        if ui.button("Reset view").clicked() {
            model.camera.set(level.home);
        }
        let (arrived, wanted) = model.arrived();
        if arrived < wanted {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(RichText::new(format!("{arrived}/{wanted}")).weak())
                .on_hover_text("Materials, shader packages and textures still on their way");
        }
        if !level.unreadable.is_empty() {
            ui.label(
                RichText::new(format!("⚠ {} unreadable meshes", level.unreadable.len()))
                    .color(Color32::LIGHT_RED),
            )
            .on_hover_text(
                level
                    .unreadable
                    .iter()
                    .map(|(index, why)| format!("mesh {index}: {why}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    });

    if let Some(why) = model.level.borrow().gpu.lock().unwrap().failure() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(Color32::RED, format!("Could not build the shader: {why}"));
        });
        return;
    }

    if model.shaded.get() && model.settings.get() {
        settings(ui, model);
    }

    if model.level.borrow().skinned {
        ui.horizontal(|ui| model.animation.ui(ui));
    }

    model.poll(ui, backend);
    model.viewport(ui);
}

/// The constants the passes past the composite are run with. Every one of these is a value a shader
/// reads and no file states, so the numbers are the user's to move rather than the viewer's to
/// settle. The occlusion ones are a guess: that buffer reports no member names, no defaults and no
/// units at all, and the lengths among them are taken as fractions of what the frame itself spans.
fn settings(ui: &mut egui::Ui, model: &Rendered) {
    let mut look = model.look.get();
    ui.horizontal_wrapped(|ui| {
        ui.label("Textures").on_hover_text(
            "Which mipmap of a model's own textures is decoded. The file arrives whole whatever \
             this says, so only memory and decoding time follow it",
        );
        egui::ComboBox::from_id_salt("mdl-detail")
            .selected_text(label(look.detail))
            .show_ui(ui, |ui| {
                for (detail, what) in DETAIL {
                    ui.selectable_value(&mut look.detail, detail, what);
                }
            });
        ui.checkbox(&mut look.antialias, "Antialias")
            .on_hover_text("Smooth the frame's edges with the game's own FXAA");
        ui.add_enabled_ui(look.antialias, |ui| {
            ui.add(egui::Slider::new(&mut look.subpix, 0.0..=1.0).text("Subpixel"))
                .on_hover_text("FXAA's own subpixel aliasing removal, at its published default");
            ui.add(egui::Slider::new(&mut look.edge, 0.03..=0.5).text("Edge"))
                .on_hover_text("How much local contrast counts as an edge, likewise");
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut look.occlude, "Occlusion")
            .on_hover_text("Shade the creases with the game's own HDAO");
        ui.add_enabled_ui(look.occlude, |ui| {
            egui::ComboBox::from_id_salt("mdl-occluder")
                .selected_text(program::OCCLUDERS[look.quality])
                .show_ui(ui, |ui| {
                    for (at, what) in program::OCCLUDERS.iter().enumerate() {
                        ui.selectable_value(&mut look.quality, at, *what);
                    }
                });
        });
    });
    ui.add_enabled_ui(look.occlude, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (value, range, name, what) in [
                (
                    &mut look.radius,
                    0.25..=4.0,
                    "Radius",
                    "What the taps the pass carries are spread by",
                ),
                (
                    &mut look.accept,
                    1.0..=200.0,
                    "Accept",
                    "How steeply a valley has to fall to count, against the distance to it",
                ),
                (
                    &mut look.reject,
                    0.005..=0.5,
                    "Reject",
                    "The fall past which two samples are no longer one surface",
                ),
                (
                    &mut look.bias,
                    0.0..=0.2,
                    "Normal bias",
                    "How far along its own normal a sample is pushed before it is compared",
                ),
                (
                    &mut look.fade,
                    0.0..=0.5,
                    "Near fade",
                    "The distance under which occlusion fades out",
                ),
                (
                    &mut look.distance,
                    0.05..=1.0,
                    "Distance",
                    "The distance past which a pixel is left alone",
                ),
                (
                    &mut look.intensity,
                    0.0..=4.0,
                    "Intensity",
                    "What the taps add up to is scaled by",
                ),
                (
                    &mut look.power,
                    0.25..=3.0,
                    "Power",
                    "The exponent it is raised to, which the pass multiplies by a second time too",
                ),
            ] {
                ui.add(egui::Slider::new(value, range).text(name))
                    .on_hover_text(what);
            }
        });
    });
    let held = model.look.get();
    if look != held {
        // Which mipmap is taken is settled when a texture is fetched, so a change means fetching
        // and decoding every one of them again. Dropping the handles is what frees what they held.
        if look.detail != held.detail {
            model.textures.borrow_mut().clear();
            model.resident.set(0);
        }
        model.look.set(look);
    }
}

/// What the ladder calls one of its rungs, or the number itself where it names none.
fn label(detail: Option<u16>) -> String {
    DETAIL
        .iter()
        .find(|(held, _)| *held == detail)
        .map_or_else(|| format!("{detail:?}"), |(_, what)| (*what).to_owned())
}

impl Rendered {
    /// Asks for whatever the model still needs, and hands what arrived to egui. Runs every frame;
    /// a slot that is already resolved costs a lookup.
    fn poll(&self, ui: &egui::Ui, backend: &Backend) {
        let level = self.level.borrow();
        if level.skinned {
            self.animation.poll(ui.ctx(), backend);
        }
        let mut slots = self.slots.borrow_mut();
        for (index, slot) in slots.iter_mut().enumerate() {
            let path = &level.materials[index];
            match slot {
                None => {
                    let files = backend.files().clone();
                    let wanted = path.clone();
                    *slot = Some(Slot::Fetching(TrackedPromise::spawn_local(async move {
                        files.read(&wanted).await
                    })));
                }
                Some(Slot::Fetching(promise)) => {
                    let Some(result) = promise.try_get() else {
                        continue;
                    };
                    *slot = Some(match result {
                        Ok(bytes) => match Material::parse(bytes) {
                            Ok(material) => Slot::Ready(Box::new(material)),
                            Err(why) => Slot::Failed(why.to_string()),
                        },
                        Err(why) => {
                            log::error!("assets/mdl: {path}: {why}");
                            Slot::Failed(why.to_string())
                        }
                    });
                    if let Some(Slot::Ready(material)) = slot
                        && let Some(table) = material.table()
                    {
                        level.gpu.lock().unwrap().queue_table(index, table.to_vec());
                    }
                }
                Some(_) => {}
            }
        }

        let mut landed = false;
        for (at, piece) in self.pieces.iter().enumerate() {
            let mut imc = piece.imc.borrow_mut();
            match &mut *imc {
                None => {
                    *imc = Some(match imc_path(&piece.path) {
                        Some(path) => {
                            let files = backend.files().clone();
                            Imc::Fetching(TrackedPromise::spawn_local(async move {
                                files.read(&path).await
                            }))
                        }
                        None => Imc::Absent,
                    });
                }
                Some(Imc::Fetching(promise)) => {
                    if let Some(result) = promise.try_get() {
                        let read = result
                            .as_ref()
                            .map_err(ToString::to_string)
                            .and_then(|bytes| {
                                ImageChange::read(Cursor::new(bytes.clone()))
                                    .map_err(|why| why.to_string())
                            });
                        *imc = Some(match read {
                            Ok(image_change) => {
                                if exclusive_variants(
                                    &image_change,
                                    imc_part(&piece.path),
                                    level.attributes[at],
                                ) {
                                    piece.variant.set(1);
                                }
                                Imc::Ready(image_change)
                            }
                            Err(why) => {
                                log::warn!("assets/mdl: {}: {why}", piece.path);
                                Imc::Absent
                            }
                        });
                        landed = true;
                    }
                }
                Some(_) => {}
            }
        }
        if landed {
            self.apply_variant();
        }

        if self.shaded.get() {
            let mut packages = self.packages.borrow_mut();
            // The packages that light the frame belong to no material, so they are asked for
            // alongside the ones the materials name.
            let wanted = slots
                .iter()
                .flatten()
                .filter_map(|slot| match slot {
                    Slot::Ready(material) => Some(material.package()),
                    _ => None,
                })
                .chain(
                    [
                        program::VIEW_POSITION,
                        program::DIRECTIONAL,
                        program::POINT,
                        program::COMPOSITE,
                        program::TONE_ADJUST,
                    ]
                    .map(str::to_owned),
                )
                // Asked for only where the viewer is drawing with them, so a frame nobody wants
                // smoothed costs no fetch at all.
                .chain(
                    self.look
                        .get()
                        .antialias
                        .then_some([program::FXAA_LUMA, program::FXAA])
                        .into_iter()
                        .flatten()
                        .map(str::to_owned),
                )
                // Of the eight readings the quality ladder offers, only the one it is set to.
                .chain(
                    self.look
                        .get()
                        .occlude
                        .then(|| {
                            [
                                program::DOWN_SCALE.to_owned(),
                                program::GATHER.to_owned(),
                                self.look.get().occluder(),
                            ]
                        })
                        .into_iter()
                        .flatten(),
                );
            for path in wanted {
                if packages.contains_key(&path) {
                    continue;
                }
                let files = backend.files().clone();
                let wanted = path.clone();
                packages.insert(
                    path,
                    Package::Fetching(TrackedPromise::spawn_local(async move {
                        files.read(&wanted).await
                    })),
                );
            }
            for (path, package) in packages.iter_mut() {
                let Package::Fetching(promise) = package else {
                    continue;
                };
                let Some(result) = promise.try_get() else {
                    continue;
                };
                *package = match result {
                    Ok(bytes) => Package::Ready(bytes.clone()),
                    Err(why) => {
                        log::error!("assets/mdl: {path}: {why}");
                        Package::Failed(why.to_string())
                    }
                };
            }

            let mut arrays = self.arrays.borrow_mut();
            for (id, path, filter) in deferred::ENGINE.into_iter().chain([deferred::GRADING]) {
                let held = arrays.entry(id).or_insert_with(|| {
                    let files = backend.files().clone();
                    Array::Fetching(TrackedPromise::spawn_local(async move {
                        files.read(path).await
                    }))
                });
                let Array::Fetching(promise) = held else {
                    continue;
                };
                let Some(result) = promise.try_get() else {
                    continue;
                };
                *held = match result
                    .as_ref()
                    .map_err(ToString::to_string)
                    .and_then(|bytes| layered(bytes, path, filter).map_err(|why| why.to_string()))
                {
                    Ok(decoded) => {
                        level.gpu.lock().unwrap().queue_array(id, decoded.clone());
                        self.graded
                            .set(self.graded.get() || id == deferred::GRADING.0);
                        Array::Ready(decoded)
                    }
                    Err(why) => {
                        log::error!("assets/mdl: {path}: {why}");
                        Array::Failed
                    }
                };
            }

            let mut parameters = self.parameters.borrow_mut();
            let mut arrived = false;
            for (base, path) in program::PARAMETERS {
                let held = parameters.entry(base).or_insert_with(|| {
                    let files = backend.files().clone();
                    Parameters::Fetching(TrackedPromise::spawn_local(async move {
                        files.read(path).await
                    }))
                });
                let Parameters::Fetching(promise) = held else {
                    continue;
                };
                let Some(result) = promise.try_get() else {
                    continue;
                };
                *held = match result
                    .as_ref()
                    .map_err(ToString::to_string)
                    .and_then(|bytes| {
                        ShaderParameters::read(Cursor::new(bytes.clone()))
                            .map_err(|why| why.to_string())
                    }) {
                    Ok(file) => Parameters::Ready(file),
                    Err(why) => {
                        log::error!("assets/mdl: {path}: {why}");
                        Parameters::Failed
                    }
                };
                arrived = true;
            }
            if arrived && let Some(values) = types(&parameters) {
                level.gpu.lock().unwrap().queue_types(values);
            }

            // The fur pass belongs to no material either, and nothing can be softened with it until
            // the frame is lit at all, so it is only worth a fetch of its own once the four above
            // are in hand and the model turns out to state a fur length.
            if self.lighting.borrow().is_some()
                && !packages.contains_key(program::FUR)
                && let Some(values) = types(&parameters)
                && slots.iter().flatten().any(|slot| match slot {
                    Slot::Ready(material) => program::furred(material, &values),
                    _ => false,
                })
            {
                let files = backend.files().clone();
                packages.insert(
                    program::FUR.to_owned(),
                    Package::Fetching(TrackedPromise::spawn_local(async move {
                        files.read(program::FUR).await
                    })),
                );
            }
        }

        let mut textures = self.textures.borrow_mut();
        let detail = self.look.get().detail;
        for slot in slots.iter().flatten() {
            let Slot::Ready(material) = slot else {
                continue;
            };
            let held: Vec<&String> = material.textures().collect();
            let bound: Vec<String> = match self.shaded.get() {
                true => material.bound().map(|(_, path)| path.to_owned()).collect(),
                false => Vec::new(),
            };
            for path in held.into_iter().chain(bound.iter()) {
                if textures.contains_key(path) {
                    continue;
                }
                if self.resident.get() >= TEXTURE_BUDGET {
                    log::warn!("assets/mdl: {path}: past this model's texture budget");
                    textures.insert(path.clone(), Texture::Absent);
                    continue;
                }
                let files = backend.files().clone();
                let wanted = path.clone();
                textures.insert(
                    path.clone(),
                    Texture::Fetching(TrackedPromise::spawn_local(async move {
                        files.read_texture(&wanted, detail).await
                    })),
                );
            }
        }
        for (path, texture) in textures.iter_mut() {
            let Texture::Fetching(promise) = texture else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            *texture = match result {
                Ok(decoded) => {
                    let size = [
                        decoded.image.width() as usize,
                        decoded.image.height() as usize,
                    ];
                    self.resident
                        .set(self.resident.get() + size[0] * size[1] * 4);
                    // Taken as premultiplied, which is the one path that copies the bytes through
                    // untouched. These are looked up channel by channel rather than composited, and
                    // a normal or mask map carrying anything but opacity in its alpha has its other
                    // three channels scaled away by the unmultiplied path.
                    let image = egui::ColorImage::from_rgba_premultiplied(
                        size,
                        decoded.image.as_flat_samples().as_slice(),
                    );
                    // Model UVs tile, and a texture bound to a surface is minified far more often
                    // than it is magnified, so this is the one place the browser wants mipmaps and
                    // repeat rather than the crisp clamped sampling a texture preview wants.
                    Texture::Ready(ui.ctx().load_texture(
                        format!("mdl:{path}"),
                        image,
                        TextureOptions {
                            magnification: egui::TextureFilter::Linear,
                            minification: egui::TextureFilter::Linear,
                            wrap_mode: egui::TextureWrapMode::Repeat,
                            mipmap_mode: Some(egui::TextureFilter::Linear),
                        },
                    ))
                }
                Err(why) => {
                    log::error!("assets/mdl: {path}: {why}");
                    Texture::Absent
                }
            };
        }
    }

    /// The model itself: an orbit camera over a paint callback.
    fn viewport(&self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }

        let level = self.level.borrow();
        let mut camera = self.camera.get();
        let pan = |camera: &mut Camera, delta: egui::Vec2| {
            let (sin_yaw, cos_yaw) = camera.yaw.sin_cos();
            let right = Vec3::new(cos_yaw, 0.0, -sin_yaw);
            let scale = camera.distance * 0.002;
            camera.target += (right * -delta.x + Vec3::Y * delta.y) * scale;
        };
        let zoom = |camera: &mut Camera, scale: f32| {
            camera.distance = (camera.distance * scale)
                .clamp(level.home.distance * 0.02, level.home.distance * 20.0);
        };

        // A second finger takes the gesture over: egui carries on reporting a primary drag through
        // one, so leaving the orbit armed would spin the model while it is being pinched.
        let touch = ui.input(|input| input.multi_touch());
        match touch.filter(|_| response.dragged()) {
            Some(touch) => {
                zoom(&mut camera, 1.0 / touch.zoom_delta);
                pan(&mut camera, touch.translation_delta);
            }
            None => {
                if response.dragged_by(egui::PointerButton::Primary) {
                    let delta = response.drag_delta();
                    camera.yaw -= delta.x * 0.01;
                    camera.pitch = (camera.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
                }
                if response.dragged_by(egui::PointerButton::Secondary) {
                    pan(&mut camera, response.drag_delta());
                }
            }
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                zoom(&mut camera, 1.0 - scroll * 0.002);
            }
        }
        self.camera.set(camera);

        // The joints move the geometry after the file stated its bounds, so where the model stands
        // has to be worked out before anything is framed or clipped against it.
        let pose = self.animation.pose(&level.bones, self.skeleton.get());
        // Carried rather than written into the camera, so a motion that walks runs in place and the
        // user's own orbit, pan and zoom still mean what they did.
        let focus = level.home.target + pose.drift;
        let reach = level.radius + pose.stretch;

        let target = camera.target + pose.drift;
        let eye = camera.eye() + pose.drift;
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);
        // Cut to the model's own bounding sphere. A fixed ratio leaves a large piece with almost no
        // depth precision where it is actually drawn.
        let span = (eye - focus).length();
        let near = (span - reach).max(reach * 0.005);
        // Past the light box's own far corner rather than past the model, since the volume a lamp
        // is drawn as is clipped by these planes whether or not anything depth tests against them.
        let far = span + reach.max(level.radius * (1.0 + LAMP_SPAN * 2.0));
        let projection = Mat4::perspective_rh_gl(FOV, rect.width() / rect.height(), near, far);

        // Fill and rim follow the camera; a fill weighted toward the eye is the whole of what keeps
        // a surface turned away from the key from reading as a silhouette. Both are built from the
        // camera's axes rather than from a fragment's view vector, which would give every pixel a
        // rig of its own and sweep it across the surface as the camera moves.
        let axes = Mat3::from_mat4(view).transpose();
        let (right, up, back) = (axes.x_axis, axes.y_axis, axes.z_axis);
        let fill = back - right * 0.5 - up * 0.2;
        let rim = -back * 0.55 + up * 0.6 - right * 0.55;
        let mut lights = [0.0; 9];
        for (slot, light) in lights.chunks_exact_mut(3).zip([KEY, fill, rim]) {
            slot.copy_from_slice(&light.normalize().to_array());
        }

        let attachments = level.gpu.lock().unwrap().attachments();
        let lighting = match self.shaded.get() {
            true => {
                self.translate(level.skinned, attachments);
                self.lighting(attachments)
            }
            false => None,
        };
        let translated = self.translated.borrow();
        let tables = self.tables.borrow();
        let slots = self.slots.borrow();
        let textures = self.textures.borrow();
        let bind = |path: &str| match textures.get(path) {
            Some(Texture::Ready(handle)) => Some(handle.id()),
            _ => None,
        };
        // One that has not answered yet, as against one that answered with nothing. The flat
        // stand-in a draw reaches for meanwhile is opaque, so a cutout authored into a normal map's
        // alpha clips nothing and the quad it was cut out of stands as a solid card.
        let pending = |path: &str| {
            !matches!(
                textures.get(path),
                Some(Texture::Ready(_) | Texture::Absent)
            )
        };
        let surfaces = level
            .meshes
            .iter()
            .map(|mesh| {
                let runs = shown(&mesh.parts);
                let Some(Some(Slot::Ready(material))) = slots.get(mesh.material) else {
                    return gpu::Surface {
                        material: mesh.material,
                        runs,
                        ..Default::default()
                    };
                };
                if !material.drawn() {
                    return gpu::Surface {
                        material: mesh.material,
                        ..Default::default()
                    };
                }
                let shaded = self.shaded.get().then(|| {
                    let passes = translated.get(&mesh.material)?.held.as_ref().ok()?;
                    if material.bound().any(|(_, path)| pending(path)) {
                        return None;
                    }
                    Some(gpu::Shaded {
                        buffer: passes.buffer.clone(),
                        depth: passes.depth.clone(),
                        resolve: passes.resolve.clone(),
                        table: tables.get(&mesh.material).cloned(),
                        textures: material
                            .bound()
                            .map(|(id, path)| (id, bind(path)))
                            .collect(),
                    })
                });
                gpu::Surface {
                    material: mesh.material,
                    shaded: shaded.flatten(),
                    runs,
                    family: material.family(),
                    normal: material.texture(Role::Normal).and_then(|path| bind(path)),
                    index: material.texture(Role::Index).and_then(|path| bind(path)),
                    mask: material.texture(Role::Mask).and_then(|path| bind(path)),
                    diffuse: material.texture(Role::Diffuse).and_then(|path| bind(path)),
                    alpha_threshold: material.alpha_threshold(),
                    diffuse_color: material.diffuse(),
                    emissive_color: material.emissive(),
                    normal_scale: material.normal_scale(),
                    cull: material.cull(),
                }
            })
            .collect();

        // The game's own shaders were compiled for a clip depth running from nought to one, and the
        // backend moves what they compute into the range GL clips against. A projection built for GL
        // would go through that move a second time and lose the near half of the frame.
        let held = Mat4::perspective_rh(FOV, rect.width() / rect.height(), near, far);

        // A cell of about half the model's radius, snapped to a one, a two or a five. Only the model
        // says what scale to rule at, and a bare decade is a tenfold jump: it leaves a piece of
        // landscape standing in one cell or a character ruled into mush.
        let cell = level.radius * 0.5;
        let decade = 10f32.powf(cell.log10().floor());
        let step = decade
            * match cell / decade {
                held if held < 1.5 => 1.0,
                held if held < 3.5 => 2.0,
                held if held < 7.5 => 5.0,
                _ => 10.0,
            };

        // The GL projection whichever path drew the frame: the game's own shaders are compiled for
        // a clip depth of nought to one and the backend moves what they write into GL's range, so
        // the depth both of them leave behind is this one's. The quad reaches past the far plane,
        // which is what leaves the fade rather than its own edge as where the grid stops.
        let grid = self.grid.get().then(|| grid::Ground {
            view_projection: (projection * view).to_cols_array(),
            // The camera carries the pose's drift and the lines do not, so a model that walks walks
            // over them.
            center: [eye.x, eye.z],
            extent: far * 1.5,
            range: [near, far],
            step,
        });

        let frame = gpu::Frame {
            view: view.to_cols_array(),
            projection: projection.to_cols_array(),
            target: self.target.get(),
            scene: program::Scene {
                view,
                projection: held,
                model: Mat4::from_scale(Vec3::splat(self.stature.get())),
                light: KEY,
                lamp: program::Lamp {
                    placement: Mat4::from_translation(
                        target + Vec3::new(0.0, level.radius, level.radius),
                    ),
                    min: Vec3::splat(-level.radius * LAMP_SPAN),
                    max: Vec3::splat(level.radius * LAMP_SPAN),
                    color: Vec3::splat(LAMP_FILL),
                    ..Default::default()
                },
                look: self.look.get(),
                customize: self.customize.get(),
                ..Default::default()
            },
            lighting,
            post: match self.shaded.get() {
                true => self.post(),
                false => None,
            },
            smoothing: match self.shaded.get() {
                true => self.smoothing(),
                false => None,
            },
            occlusion: match self.shaded.get() {
                true => self.occlusion(),
                false => None,
            },
            eye: eye.to_array(),
            lights,
            surfaces,
            joints: pose.joints,
            debug: self.debug.get(),
            grid,
        };

        // Drawn with no depth test, which is what makes it an overlay rather than a rig buried in
        // the mesh it poses.
        let overlay = self.skeleton.get().then(|| {
            self.overlay.lock().unwrap().replace(pose.skeleton);
            (self.overlay.clone(), (projection * view).to_cols_array())
        });

        // The context is taken from the painter rather than captured: `glow::Context` is neither
        // `Send` nor `Sync` on wasm, and a callback has to be both.
        let model = level.gpu.clone();
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                model
                    .lock()
                    .unwrap()
                    .draw(painter.gl(), painter, &frame, &info);
                if let Some((bones, view_projection)) = &overlay {
                    bones
                        .lock()
                        .unwrap()
                        .draw(painter.gl(), painter, view_projection, false);
                }
            })),
        });
    }

    /// How much of what the model needs has landed, against how much it asked for. A material names
    /// the package and textures it wants only once it has arrived itself, so the total grows as the
    /// fetches resolve rather than being known up front.
    fn arrived(&self) -> (usize, usize) {
        let slots = self.slots.borrow();
        let packages = self.packages.borrow();
        let textures = self.textures.borrow();
        let ready = slots
            .iter()
            .flatten()
            .filter(|slot| !matches!(slot, Slot::Fetching(_)))
            .count()
            + packages
                .values()
                .filter(|held| !matches!(held, Package::Fetching(_)))
                .count()
            + textures
                .values()
                .filter(|held| !matches!(held, Texture::Fetching(_)))
                .count();
        // A slot the model has not asked for yet still owes an answer, so every material the level
        // names counts against the total whether or not it is in flight.
        (ready, slots.len() + packages.len() + textures.len())
    }

    /// What the channel row offers: the translated shaders' own names for their targets, and the
    /// frame the composite resolves once the passes that make it have arrived.
    fn channels(&self) -> Vec<(usize, String)> {
        let mut held: Vec<(usize, String)> = self
            .translated
            .borrow()
            .values()
            .filter_map(|held| held.held.as_ref().ok())
            .find_map(|passes| passes.buffer.first())
            .map(|buffer| buffer.names.iter().cloned().enumerate().collect())
            .unwrap_or_default();
        if !held.is_empty() && self.lighting.borrow().is_some() {
            held.push((gpu::LIT, "Lit".to_owned()));
        }
        held
    }

    /// The pass that grades the resolved frame, translated once its shader has arrived. Withheld
    /// until the table it reads has landed too: a pass drawn against the flat stand-in would grade
    /// every pixel toward the one grey it answers with.
    fn post(&self) -> Option<Arc<program::Program>> {
        if !self.graded.get() {
            return None;
        }
        if let Some(held) = self.post.borrow().as_ref() {
            return Some(held.clone());
        }
        let mut packages = self.packages.borrow_mut();
        let built = match packages.get(program::TONE_ADJUST) {
            Some(Package::Ready(bytes)) => {
                program::Program::posteffect(bytes, program::POST_VERTEX)
            }
            _ => return None,
        };
        // Kept as a failure rather than tried again: the file will not translate differently on the
        // next frame, and the pass is skipped from here on.
        let built = match built {
            Ok(held) => Arc::new(held),
            Err(why) => {
                log::error!("assets/mdl: {}: {why}", program::TONE_ADJUST);
                packages.insert(program::TONE_ADJUST.to_owned(), Package::Failed(why));
                return None;
            }
        };
        drop(packages);
        *self.post.borrow_mut() = Some(built.clone());
        Some(built)
    }

    /// The pair that smooths the graded frame's edges, translated once both shaders have arrived.
    fn smoothing(&self) -> Option<Arc<gpu::Smoothing>> {
        if !self.look.get().antialias {
            return None;
        }
        if let Some(held) = self.smoothing.borrow().as_ref() {
            return Some(held.clone());
        }
        let packages = self.packages.borrow();
        let held = |path: &str| {
            let Some(Package::Ready(bytes)) = packages.get(path) else {
                return None;
            };
            program::Program::posteffect(bytes, program::POST_VERTEX)
                .inspect_err(|why| log::warn!("assets/mdl: {path}: {why}"))
                .ok()
                .map(Arc::new)
        };
        let built = gpu::Smoothing {
            luma: held(program::FXAA_LUMA)?,
            fxaa: held(program::FXAA)?,
        };
        drop(packages);
        let built = Arc::new(built);
        *self.smoothing.borrow_mut() = Some(built.clone());
        Some(built)
    }

    /// The chain that occludes the frame, translated once its three shaders have arrived. Rebuilt
    /// where the quality changed, since that is a file of its own.
    fn occlusion(&self) -> Option<Arc<gpu::Occlusion>> {
        let look = self.look.get();
        if !look.occlude {
            return None;
        }
        if let Some((quality, held)) = self.occlusion.borrow().as_ref()
            && *quality == look.quality
        {
            return Some(held.clone());
        }
        let packages = self.packages.borrow();
        let held = |path: &str, vertex| {
            let Some(Package::Ready(bytes)) = packages.get(path) else {
                return None;
            };
            program::Program::posteffect(bytes, vertex)
                .inspect_err(|why| log::warn!("assets/mdl: {path}: {why}"))
                .ok()
                .map(Arc::new)
        };
        let built = gpu::Occlusion {
            scale: held(program::DOWN_SCALE, program::POST_VERTEX)?,
            gather: held(program::GATHER, program::GATHER_VERTEX)?,
            occlude: held(&look.occluder(), program::POST_VERTEX)?,
        };
        drop(packages);
        let built = Arc::new(built);
        *self.occlusion.borrow_mut() = Some((look.quality, built.clone()));
        Some(built)
    }

    /// The passes that light the G-buffer, translated once their packages have arrived. They are the
    /// same whatever is being drawn, so they are built once and kept.
    fn lighting(&self, attachments: usize) -> Option<Arc<gpu::Lighting>> {
        if self.lighting.borrow().is_some() {
            self.soften(attachments);
            return self.lighting.borrow().clone();
        }
        let packages = self.packages.borrow();
        let held = |path: &str, pass| {
            let Some(Package::Ready(bytes)) = packages.get(path) else {
                return None;
            };
            program::Program::screen(bytes, pass, attachments)
                .inspect_err(|why| log::warn!("assets/mdl: {path}: {why}"))
                .ok()
                .map(Arc::new)
        };
        let built = gpu::Lighting {
            position: held(program::VIEW_POSITION, program::Pass::Lighting)?,
            directional: held(program::DIRECTIONAL, program::Pass::Lighting)?,
            point: held(program::POINT, program::Pass::Lamp)?,
            // A model stands under one studio light of this viewer's own, which is a point.
            spot: None,
            fur: None,
            composite: held(program::COMPOSITE, program::Pass::Composite)?,
        };
        drop(packages);
        let built = Arc::new(built);
        *self.lighting.borrow_mut() = Some(built.clone());
        Some(built)
    }

    /// Takes the fur pass up on whichever frame its package arrives on, the frame having lit without
    /// it until then. One that arrived and would not translate is marked failed rather than
    /// translated again every frame, which costs a whole one.
    fn soften(&self, attachments: usize) {
        let lit = self.lighting.borrow().clone();
        let Some(lighting) = lit.filter(|held| held.fur.is_none()) else {
            return;
        };
        let mut packages = self.packages.borrow_mut();
        let Some(Package::Ready(bytes)) = packages.get(program::FUR) else {
            return;
        };
        let fur = match program::Program::screen(bytes, program::Pass::Fur, attachments) {
            Ok(held) => Arc::new(held),
            Err(why) => {
                log::warn!("assets/mdl: {}: {why}", program::FUR);
                packages.insert(program::FUR.to_owned(), Package::Failed(why));
                return;
            }
        };
        drop(packages);
        *self.lighting.borrow_mut() = Some(Arc::new(gpu::Lighting {
            fur: Some(fur),
            ..(*lighting).clone()
        }));
    }

    /// Translates every ready material's passes, again where the context's own limit changed how
    /// many of the G-buffer's targets one reading can write.
    fn translate(&self, skinned: bool, attachments: usize) {
        let slots = self.slots.borrow();
        let packages = self.packages.borrow();
        let mut translated = self.translated.borrow_mut();
        let mut tables = self.tables.borrow_mut();
        // The keys the engine sets rather than the material: a mesh carrying bone indices is one the
        // game would draw through the skinning variant.
        let set: &[(u32, u32)] = match skinned {
            true => &[
                (TRANSFORM_VIEW, TRANSFORM_VIEW_SKIN),
                (GET_NORMAL_MAP, GET_NORMAL_MAP_ON),
                (APPLY_ALPHA_CLIP, APPLY_ALPHA_CLIP_ON),
            ],
            false => &[
                (GET_NORMAL_MAP, GET_NORMAL_MAP_ON),
                (APPLY_ALPHA_CLIP, APPLY_ALPHA_CLIP_ON),
            ],
        };
        for (index, slot) in slots.iter().enumerate() {
            let Some(Slot::Ready(material)) = slot else {
                continue;
            };
            if translated
                .get(&index)
                .is_some_and(|held| held.attachments == attachments)
            {
                continue;
            }
            let Some(Package::Ready(bytes)) = packages.get(&material.package()) else {
                continue;
            };
            let build = |pass, at| {
                program::Program::build(
                    bytes,
                    material,
                    set,
                    pass,
                    program::SUB_VIEW_MAIN,
                    at,
                    attachments,
                )
            };
            let mut passes = Passes::default();
            if let Ok(first) = build(program::Pass::Buffer, 0) {
                let pages = first.outputs.len().div_ceil(attachments.max(1)).max(1);
                passes.buffer.push(Arc::new(first));
                passes.buffer.extend(
                    (1..pages).filter_map(|at| build(program::Pass::Buffer, at).ok().map(Arc::new)),
                );
                passes.depth = build(program::Pass::Depth, 0).ok().map(Arc::new);
            }
            // A package carrying a composite of its own resolves itself with it. The screen-wide
            // pass is `bg`'s, and `bg` reserves values past one in the second target as the sign
            // that a pixel keeps its specular color in the fifth; a character writes a luminance
            // there that reaches one of its own accord, and is then read as that.
            passes.resolve = build(program::Pass::Composite, 0)
                .or_else(|_| build(program::Pass::CompositeBlended, 0))
                .ok()
                .map(Arc::new);
            let held = match passes.buffer.is_empty() && passes.resolve.is_none() {
                true => Err("this material's keys reach no pass that draws it".into()),
                false => Ok(passes),
            };
            if let Err(why) = &held {
                log::warn!("assets/mdl: {}: {why}", material.package());
            }
            translated.insert(index, Translated { attachments, held });
            if let Some((values, columns, rows)) =
                material.held().color_table().and_then(program::table)
            {
                tables
                    .entry(index)
                    .or_insert_with(|| Arc::new((values, columns, rows)));
            }
        }
    }

    /// Defaults every part's visibility from the picked variant's attribute mask. Cheap enough to
    /// call on every arrival and every pick: it only sets cells, never rebuilds the level.
    ///
    /// A part gated past the ten bits an imc entry carries is one the file cannot speak about, so it
    /// draws whatever the variant says: a model may declare far more attributes than that. So is one
    /// whose entry enables nothing, which is what a racial outfit states for every slot but the body.
    /// The parts that gates are the seams between slots, and holding them back leaves a character's
    /// breeches ending mid-thigh where its boots start at the knee.
    fn apply_variant(&self) {
        let masks: Vec<Option<u32>> = self.pieces.iter().map(Piece::mask).collect();
        let level = self.level.borrow();
        let hidden = self.hidden.borrow();
        for (mask, part) in level.meshes.iter().flat_map(|mesh| {
            let mask = masks[mesh.piece];
            mesh.parts.iter().map(move |part| (mask, part))
        }) {
            // A part the tab has switched off is off whatever the variant says, which is how a face
            // draws one of the features it declares rather than every one of them at once.
            if part
                .attributes
                .split(", ")
                .any(|name| hidden.contains(name))
            {
                part.shown.set(false);
                continue;
            }
            let gated = part.mask & IMC_ATTRIBUTES;
            part.shown
                .set(mask.is_none_or(|mask| gated == 0 || mask == 0 || gated & mask != 0));
        }
    }

    /// Rewrites every touched mesh's indices from the file's own, so switching a shape off restores
    /// what it replaced and two shapes over the same mesh both land.
    fn apply(&self) {
        let level = self.level.borrow();
        let enabled = self.shapes.borrow();
        let mut rewritten: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
        for shape in level
            .shapes
            .iter()
            .filter(|shape| enabled.contains(&shape.name))
        {
            for (mesh, values) in &shape.rewrites {
                let indices = rewritten
                    .entry(*mesh)
                    .or_insert_with(|| level.meshes[*mesh].base.clone());
                for (offset, vertex) in values {
                    if let Some(held) = indices.get_mut(usize::from(*offset)) {
                        *held = *vertex;
                    }
                }
            }
        }
        // A mesh a shape has just stopped touching still holds that shape's indices, so every mesh
        // any shape reaches is uploaded rather than only the ones still rewritten.
        let mut gpu = level.gpu.lock().unwrap();
        for mesh in level
            .shapes
            .iter()
            .flat_map(|shape| &shape.rewrites)
            .map(|(mesh, _)| *mesh)
            .collect::<BTreeSet<_>>()
        {
            let indices = rewritten
                .remove(&mesh)
                .unwrap_or_else(|| level.meshes[mesh].base.clone());
            gpu.queue_indices(mesh, indices);
        }
    }

    /// Draws another detail level of the same files.
    fn switch(&self, lod: u8) {
        let paths: Vec<&str> = self
            .pieces
            .iter()
            .map(|piece| piece.path.as_str())
            .collect();
        match level_of(&self.pieces, lod) {
            Ok(level) => {
                self.lod.set(lod);
                self.rebuild(level);
            }
            Err(why) => log::error!(
                "assets/mdl: {}: detail level {lod}: {why}",
                paths.join(" + ")
            ),
        }
    }

    /// How the character was made: the colours its shaders tint with, the attributes its face draws
    /// and the shape keys that deform it. Taken together so a pick costs one pass over the parts.
    pub fn made(
        &self,
        customize: program::Customize,
        hidden: BTreeSet<String>,
        shapes: BTreeSet<String>,
        stature: f32,
    ) {
        self.customize.set(customize);
        self.stature.set(stature);
        *self.hidden.borrow_mut() = hidden;
        let changed = *self.shapes.borrow() != shapes;
        *self.shapes.borrow_mut() = shapes;
        self.apply_variant();
        if changed {
            self.apply();
        }
    }

    /// Poses the character out of a different pack, which is what picking an emote is.
    pub fn play(&self, path: &str) {
        self.animation.play(path);
    }

    /// Puts a different set of files on the same character, which is what a change of clothes is.
    /// The camera, the rig and the motion it is playing all stay where they are; the rig is rebuilt
    /// only where the body under the clothes changed.
    pub fn redress(&mut self, parts: &[Source]) -> Result<()> {
        let first = parts.first().context("a model of no files")?;
        let pieces: Vec<_> = parts.iter().map(Piece::new).collect();
        let drawn = drawn_levels(&pieces)?;
        let lod = match drawn[usize::from(self.lod.get())] {
            true => self.lod.get(),
            false => 0,
        };
        let level = level_of(&pieces, lod)?;

        if skin::code(&first.path)
            != self
                .pieces
                .first()
                .and_then(|piece| skin::code(&piece.path))
        {
            self.animation = skin::Animation::new(parts.iter().map(|part| part.path.as_str()));
        }
        self.animation
            .rewear(parts.iter().map(|part| part.path.as_str()));
        self.pieces = pieces;
        self.drawn = drawn;
        self.lod.set(lod);
        self.rebuild(level);
        Ok(())
    }

    /// Takes up a level built from the pieces the model now holds. Everything already fetched is
    /// kept and matched to the new geometry by material path, so nothing is asked for twice and
    /// nothing pops. Index is not a key that survives this: merging or changing a piece renumbers
    /// the materials, and an entry carried across by index would draw one material's geometry
    /// through another's shader.
    fn rebuild(&self, level: Level) {
        let mut slots = self.slots.borrow_mut();
        let mut translated = self.translated.borrow_mut();
        let mut tables = self.tables.borrow_mut();
        let was = std::mem::take(&mut self.level.borrow_mut().materials);
        let mut held: BTreeMap<String, Kept> = was
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let slot = slots.get_mut(index).and_then(Option::take);
                (
                    path,
                    (slot, translated.remove(&index), tables.remove(&index)),
                )
            })
            .collect();
        translated.clear();
        tables.clear();
        slots.clear();
        for (index, path) in level.materials.iter().enumerate() {
            let (slot, program, table) = held.remove(path).unwrap_or_default();
            translated.extend(program.map(|program| (index, program)));
            tables.extend(table.map(|table| (index, table)));
            slots.push(slot);
        }
        // The new level's context has no color tables of its own, and a material kept from the old
        // one never transitions again to hand one over.
        for (index, slot) in slots.iter().enumerate() {
            if let Some(Slot::Ready(material)) = slot
                && let Some(table) = material.table()
            {
                level.gpu.lock().unwrap().queue_table(index, table.to_vec());
            }
        }
        // Nor the shader type table, nor the engine's own texture arrays, and the files all of
        // them are built from have already arrived.
        if let Some(values) = types(&self.parameters.borrow()) {
            level.gpu.lock().unwrap().queue_types(values);
        }
        for (id, bytes) in self.arrays.borrow().iter() {
            if let Array::Ready(bytes) = bytes {
                level.gpu.lock().unwrap().queue_array(*id, bytes.clone());
            }
        }

        drop((slots, translated, tables));
        *self.level.borrow_mut() = level;
        self.apply();
        self.apply_variant();
    }

    pub fn details_ui(&self, ui: &mut egui::Ui, follow: &mut Option<String>) {
        let mut picked = None;
        let mut toggled = None;
        let mut picked_shape = None;
        let mut picked_variant = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let level = self.level.borrow();
            facts(ui, "mdl_identity", &level.identity);
            // A file drawing at one detail level has nothing to pick between.
            if self.drawn.iter().filter(|drawn| **drawn).count() > 1 {
                ui.add_space(8.0);
                section(ui, "Detail");
                let lod = self.lod.get();
                ui.horizontal(|ui| {
                    for (level, label) in [(0, "High"), (1, "Medium"), (2, "Low")] {
                        let picker = ui.add_enabled(
                            self.drawn[usize::from(level)],
                            egui::Button::selectable(lod == level, label),
                        );
                        if picker.clicked() && lod != level {
                            picked = Some(level);
                        }
                    }
                });
            }
            if !level.shapes.is_empty() {
                ui.add_space(8.0);
                section(ui, "Shapes");
                let enabled = self.shapes.borrow();
                let on = |at: usize| enabled.contains(&level.shapes[at].name);
                let hover = |at: usize| {
                    let shape = &level.shapes[at];
                    format!("{}\n{} meshes rewritten", shape.name, shape.rewrites.len())
                };
                // Clicking the variant already showing is what turns its category off, so a
                // category needs no entry of its own for having nothing applied.
                let chip = |ui: &mut egui::Ui, at: usize, label: &str| {
                    ui.selectable_label(on(at), label)
                        .on_hover_text(hover(at))
                        .clicked()
                };
                for (index, group) in level.groups.iter().enumerate() {
                    if group.category.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(&group.category).weak());
                    ui.horizontal_wrapped(|ui| {
                        for (at, variant) in &group.variants {
                            if chip(ui, *at, variant) {
                                picked_shape = Some((index, (!on(*at)).then_some(*at)));
                            }
                        }
                    });
                }
                // Whatever the file names without a variant, which is most of what a model
                // deforms. Each stands on its own, so they share one row rather than taking a
                // heading each.
                if level.groups.iter().any(|group| group.category.is_empty()) {
                    ui.horizontal_wrapped(|ui| {
                        for (index, group) in level.groups.iter().enumerate() {
                            if !group.category.is_empty() {
                                continue;
                            }
                            let (at, name) = &group.variants[0];
                            if chip(ui, *at, name) {
                                picked_shape = Some((index, (!on(*at)).then_some(*at)));
                            }
                        }
                    });
                }
            }

            for (at, piece) in self.pieces.iter().enumerate() {
                let Some(count) = piece.variants().filter(|count| *count > 0) else {
                    continue;
                };
                ui.add_space(8.0);
                section(ui, "Variant");
                if self.pieces.len() > 1 {
                    ui.label(RichText::new(crate::utils::file_name(&piece.path)).weak());
                }
                let current = piece.variant.get();
                ui.horizontal_wrapped(|ui| {
                    for variant in 0..=count {
                        if ui
                            .selectable_label(current == variant, variant.to_string())
                            .clicked()
                            && current != variant
                        {
                            picked_variant = Some((at, variant));
                        }
                    }
                });
            }

            if level.skinned {
                ui.add_space(8.0);
                self.animation.details_ui(ui, follow);
            }

            ui.add_space(8.0);
            section(ui, "Meshes");
            for (index, mesh) in level.meshes.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    let drawn = mesh.parts.iter().any(|part| part.shown.get());
                    if ui
                        .selectable_label(drawn, RichText::new(format!("Mesh {index}")).weak())
                        .on_hover_text(format!(
                            "{}\n{} triangles",
                            crate::utils::file_name(&level.materials[mesh.material]),
                            mesh.triangles
                        ))
                        .clicked()
                    {
                        toggled = Some((index, None));
                    }
                    for (part, held) in mesh.parts.iter().enumerate() {
                        let label = match held.attributes.is_empty() {
                            true => part.to_string(),
                            false => held.attributes.clone(),
                        };
                        if ui.selectable_label(held.shown.get(), label).clicked() {
                            toggled = Some((index, Some(part)));
                        }
                    }
                });
            }
            ui.add_space(8.0);
            section(ui, "Materials");
            let slots = self.slots.borrow();
            for (index, path) in level.materials.iter().enumerate() {
                if link(ui, crate::utils::file_name(path), path) {
                    *follow = Some(path.clone());
                }
                match slots.get(index).and_then(Option::as_ref) {
                    Some(Slot::Ready(material)) => {
                        ui.label(RichText::new(material.summary()).weak());
                    }
                    Some(Slot::Failed(why)) => {
                        ui.label(RichText::new(why).color(Color32::LIGHT_RED));
                    }
                    _ => {
                        ui.label(RichText::new("loading").weak());
                    }
                }
                ui.add_space(4.0);
            }
        });
        if let Some((mesh, part)) = toggled {
            let level = self.level.borrow();
            let parts = &level.meshes[mesh].parts;
            match part {
                Some(part) => parts[part].shown.set(!parts[part].shown.get()),
                None => {
                    let hide = parts.iter().any(|part| part.shown.get());
                    for part in parts {
                        part.shown.set(!hide);
                    }
                }
            }
        }
        if let Some((piece, variant)) = picked_variant {
            self.pieces[piece].variant.set(variant);
            self.apply_variant();
        }
        if let Some((group, variant)) = picked_shape {
            {
                let level = self.level.borrow();
                let mut enabled = self.shapes.borrow_mut();
                for (at, _) in &level.groups[group].variants {
                    enabled.remove(&level.shapes[*at].name);
                }
                if let Some(at) = variant {
                    enabled.insert(level.shapes[at].name.clone());
                }
            }
            self.apply();
        }
        if let Some(lod) = picked {
            self.switch(lod);
        }
    }
}
