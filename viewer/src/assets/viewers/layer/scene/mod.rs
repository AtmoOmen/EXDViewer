//! A zone's layers placed in space, flown through.
//!
//! Every `BgPart` is drawn at its own transform, and a `SharedGroup` is another file's tree read
//! under the transform that placed it. Composition is the ordinary one: a node's matrix is
//! translation times rotation times scale, the stored Euler triple turns about X first, then Y,
//! then Z, and a child is its parent's matrix times its own.
//!
//! Nothing is fetched up front. Files, models, materials and textures are asked for a few at a time
//! and nearest first, and the view draws whatever has arrived, so a zone fills in around the camera
//! rather than appearing at once. What is asked for at all is bounded by a load distance the user
//! sets: past it an instance is neither drawn nor fetched.
//!
//! The shading is the game's own. Every surface goes through the package its material names, into
//! the same deferred frame the model viewer draws into, and the lights the zone places are drawn as
//! the volumes its `.lcb` clips them against.

mod ambient;
mod gpu;
mod preset;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use egui::{Color32, RichText, ScrollArea, Sense, TextureHandle, TextureOptions};
use glam::{Mat3, Mat4, Quat, Vec3};
use half::f16;
use ironworks::file::layer::{InstanceData, LayerGroup, LightKind, SceneTimeline, Transform};
use ironworks::file::tmb;
use ironworks::file::mdl::ModelContainer;
use ironworks::file::shpk::ShaderPackage;
use ironworks::file::spm::ShaderParameters;
use ironworks::file::{
    File, ggd, gzd, layer, lcb, lgb::LayerGroupFile, sgb::SharedGroupFile, svb, tera,
};

use super::super::mdl;
use super::super::{facts, section};
use super::Source;
use crate::assets::deps::Deps;
use crate::backend::Backend;
use crate::data::DecodedTexture;
use crate::utils::TrackedPromise;

use mdl::material::Material;
use mdl::program;

/// Vertical field of view.
const FOV: f32 = 55.0_f32.to_radians();

/// How deep a shared group may hold another. Files reach four; the cap guards against a cycle
/// rather than limiting anything real.
const DEPTH: u8 = 8;

/// Longest edge a scene's textures are decoded to. Smaller than the model viewer's: a zone binds
/// hundreds of materials rather than one model's handful, over the same connection.
const TEXTURE_SIZE: u16 = 256;

/// Decoded texture bytes one scene may hold. Past it the rest of its surfaces draw untextured.
const TEXTURE_BUDGET: usize = 128 << 20;

/// Longest edge a grass color map is decoded to. Over the cap above, since a map holds its tiles
/// side by side and a blade reads one of them.
const GRASS_SIZE: u16 = 1024;

/// Tiles a grass color map is laid out in, which a placement's own profile picks between.
const TILES: u8 = 8;

/// Blades a scene will stand up, each a quad of its own. Past it a grid's own are left out whole.
const BLADES: usize = 200_000;

/// Lights the frame draws at once. Every one is a pass of its own over the volume it reaches, so a
/// zone's whole set would cost more than it shows; the nearest are kept.
const LAMPS: usize = 256;

/// The scene key deciding whether a background shader reads the normal map at all. A package
/// defaults it to off, and the variant that answer selects samples no normal map, so the frame it
/// writes is the geometry's own.
const GET_NORMAL_MAP: u32 = 0xcbdf_d5ec;
const GET_NORMAL_MAP_ON: u32 = 0xd999_4ef1;

/// The scene key deciding whether a shader clips against its own alpha threshold. A package defaults
/// it to off, and the variant that answer selects carries no clip at all, so a material's cutout
/// leaves the geometry it was authored over standing.
const APPLY_ALPHA_CLIP: u32 = 0xdcfc_844e;
const APPLY_ALPHA_CLIP_ON: u32 = 0x59c4_e6db;

/// `ApplyDetailMap`, and the value that lays the tiled arrays over a surface. Left at the package's
/// own default a wall is its albedo and nothing finer however close the camera stands.
const APPLY_DETAIL_MAP: u32 = 0x6313_fd87;
const APPLY_DETAIL_MAP_ON: u32 = 0x7a3d_9efd;

/// `ApplyWavingAnim`, and the value that lets the wind reach a surface. Only the models whose own
/// header allows it are drawn through the variant it selects.
const APPLY_WAVING_ANIM: u32 = 0x105c_6a52;
const APPLY_WAVING_ANIM_ON: u32 = 0xf801_b859;

/// The keys the engine sets rather than the material. A package that declares none of them resolves
/// exactly as it did, since a key the package never declares is never looked up.
const KEYS: [(u32, u32); 3] = [
    (GET_NORMAL_MAP, GET_NORMAL_MAP_ON),
    (APPLY_ALPHA_CLIP, APPLY_ALPHA_CLIP_ON),
    (APPLY_DETAIL_MAP, APPLY_DETAIL_MAP_ON),
];

/// What a light is worth where the zone states no box for it. Nothing in the placement carries the
/// reach: the file's own `range` is one in nearly every light a zone places.
const REACH: f32 = 6.0;

/// How fast a shared group's timeline runs. No file names the unit its keys are stated in; this is
/// the rate the game's own timelines are read at.
const TICKS: f32 = 30.0;

/// Requests of each kind in flight at once.
const FILES: usize = 12;
const PACKAGES: usize = 4;
const MODELS: usize = 24;
const MATERIALS: usize = 16;
const TEXTURES: usize = 24;

/// Files parsed and models decoded in one frame. Both happen on the thread that draws, so they are
/// spread rather than done as they arrive.
const PARSES: usize = 2;
const DECODES: usize = 2;

/// How far the eye moves before the instance buffers are written again.
const STEP: f32 = 8.0;

/// How large an instance has to look to be worth its highest detail level, and its middle one, as a
/// fraction of the distance to it.
const DETAIL: [f32; 2] = [0.04, 0.012];

/// What the load distance may be set to, and where it starts.
const NEAREST: f32 = 400.0;
const FURTHEST: f32 = 16000.0;
const LOADED: f32 = 4000.0;

/// How far the eye travels a second, before the user's multiplier.
const SPEED: f32 = 100.0;

/// How much of the fitted reach the opening view stands back by.
const MARGIN: f32 = 1.4;

/// The share of instances a fit is taken over. A zone holds placements a million units from
/// anything else, and a fit that covered them would leave the zone itself a speck.
const BULK: f32 = 0.9;

/// How far a terrain plate reaches, for culling. Nothing in the terrain file states it, and an
/// overestimate only loads a plate sooner than it needs to.
const PLATE: f32 = 128.0;

/// How many grass grids are asked for at once. A zone sorts hundreds of them and each is small, so
/// what this bounds is how much of the fetch budget grass takes from the models and materials.
const GRIDS: usize = 4;

/// One layer of one of the scene's files, as the picker offers it.
struct Layer {
    name: String,
    /// The file it came from, where a level merged several.
    origin: Option<String>,
    /// What the file says about whether it draws, which is what the picker starts at.
    visible: bool,
    festival: u16,
    shown: bool,
    placements: usize,
}

/// A placement one or more timelines move rather than leaving where the file put it: each motion
/// along the way with whatever fixed transform stands in front of it, and the tail below the last.
///
/// A chain rather than a single motion, since a group a timeline turns can hold another the same
/// timeline system turns again: composing them is what keeps a part turning with its parent instead
/// of against it.
struct Driven {
    chain: Vec<(usize, Mat4)>,
    tail: Mat4,
}

/// What one node of a shared group's timeline does to it, as nine curves over a span. Nothing states
/// the unit of that span; the game's own timelines run at thirty to the second.
struct Motion {
    curves: Vec<(tmb::Channel, tmb::Curve)>,
    duration: f32,
}

impl Motion {
    /// Where the node stands at a time, which the curves state outright rather than as an offset
    /// from wherever the file placed it.
    fn at(&self, time: f32) -> Mat4 {
        let span = self.duration.max(1.0);
        let along = time.rem_euclid(span);
        let mut turn = Vec3::ZERO;
        let mut shift = Vec3::ZERO;
        let mut size = Vec3::ONE;
        for (channel, curve) in &self.curves {
            let Some(held) = curve.at(along) else {
                continue;
            };
            let lane = |into: &mut Vec3, at: usize| into[at] = held;
            match channel {
                tmb::Channel::TranslationX => lane(&mut shift, 0),
                tmb::Channel::TranslationY => lane(&mut shift, 1),
                tmb::Channel::TranslationZ => lane(&mut shift, 2),
                tmb::Channel::RotationX => lane(&mut turn, 0),
                tmb::Channel::RotationY => lane(&mut turn, 1),
                tmb::Channel::RotationZ => lane(&mut turn, 2),
                tmb::Channel::ScaleX => lane(&mut size, 0),
                tmb::Channel::ScaleY => lane(&mut size, 1),
                tmb::Channel::ScaleZ => lane(&mut size, 2),
            }
        }
        Mat4::from_scale_rotation_translation(
            size,
            Quat::from_euler(
                glam::EulerRot::XYZ,
                turn.x.to_radians(),
                turn.y.to_radians(),
                turn.z.to_radians(),
            ),
            shift,
        )
    }
}

/// One `BgPart`, in world space.
#[derive(Clone)]
struct Placement {
    model: usize,
    transform: Mat4,
    /// Set where a timeline moves this, in which case the transform above is only where it starts.
    driven: Option<Rc<Driven>>,
    center: Vec3,
    /// The instance's own bounding sphere, which the file states in world units.
    radius: f32,
    /// Past this an instance stops drawing whatever the load distance is. Zero means never.
    fade: f32,
    layer: usize,
    /// How the zone's own `.svb` reaches this part, the way an `.lcb` reaches a light.
    key: (u32, [u8; 4]),
}

enum State {
    /// Wanted, but nothing has been asked for yet.
    Wanted,
    Fetching(TrackedPromise<Result<(Vec<u8>, u8)>>),
    Decoding(Vec<u8>, u8),
    Ready,
    Failed,
}

struct Model {
    path: String,
    state: State,
    /// Which detail levels hold geometry.
    drawn: [bool; 3],
    /// Per detail level, the scene material each of its meshes uses.
    meshes: Vec<Vec<usize>>,
    /// Placements drawing this model.
    instances: usize,
    /// How far the nearest of them was at the last rebuild, which is the order models are asked for
    /// in.
    nearest: f32,
    /// The finest detail level any of them would draw, and the level last asked for. A file is read
    /// again only where the eye has come close enough to want more of it than was taken.
    finest: u8,
    asked: u8,
}

enum Slot {
    Wanted,
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Box<Material>),
    Failed,
}

/// A shader package, which many materials name the same one of. A fetch answers whether the
/// bytecode came with it.
enum Package {
    Wanted,
    Fetching(TrackedPromise<Result<(Vec<u8>, bool)>>),
    Ready(Vec<u8>),
    Failed,
}

/// The blobs one read of a package answered with, each under the shader it belongs to.
type Blobbed = TrackedPromise<Result<Vec<(u32, Vec<u8>)>>>;

/// A package whose bytecode was left behind: where each of its shaders' blobs sits in the file, and
/// which of them the surfaces read so far have asked for.
struct Blobs {
    spans: Vec<std::ops::Range<u32>>,
    arrived: BTreeSet<u32>,
    wanted: BTreeSet<u32>,
    fetching: Option<Blobbed>,
}

impl Blobs {
    fn read(package: &ShaderPackage) -> Self {
        let base = package.blobs_offset() as u32;
        Self {
            spans: package
                .shaders()
                .iter()
                .map(|shader| {
                    let at = base + shader.blob_offset();
                    at..at + shader.blob_size()
                })
                .collect(),
            arrived: BTreeSet::new(),
            wanted: BTreeSet::new(),
            fetching: None,
        }
    }
}

/// The draws a zone makes of a surface, which is what a package is read for. A shader outside these
/// is never translated, so its bytecode is never asked for.
const DRAWS: [(program::Pass, u32); 6] = [
    (program::Pass::Buffer, program::SUB_VIEW_MAIN),
    (program::Pass::Blended, program::SUB_VIEW_MAIN),
    (program::Pass::Depth, program::SUB_VIEW_MAIN),
    (program::Pass::Depth, program::SUB_VIEW_SHADOW_0),
    (program::Pass::Water, program::SUB_VIEW_MAIN),
    (program::Pass::BlendedLighting, program::SUB_VIEW_MAIN),
];

/// Whether a package is one of the surfaces that blend themselves into the frame.
fn wet_name(held: &str) -> bool {
    [
        "water.shpk",
        "river.shpk",
        "crystal.shpk",
        "lightshaft.shpk",
        "verticalfog.shpk",
    ]
    .iter()
    .any(|one| held.ends_with(one))
}

/// One material's shaders, and how much of the G-buffer they were translated for.
struct Translated {
    attachments: usize,
    buffer: Vec<Arc<program::Program>>,
    depth: Option<Arc<program::Program>>,
    shadow: Option<Arc<program::Program>>,
    resolve: Option<Arc<program::Program>>,
}

/// One light the zone places. The box it is clipped against is stated in its own space, so the
/// placement carries where it stands and the box how far it carries.
struct Light {
    placement: Mat4,
    /// How far it stays at full strength, which its own record states.
    range: f32,
    center: Vec3,
    min: Vec3,
    max: Vec3,
    color: Vec3,
    kind: program::LampKind,
    /// Which way it throws, in world space.
    direction: Vec3,
    /// The cosine its cone is cut at.
    cone: f32,
    /// How the zone's own `.lcb` reaches this light: the instance at the top of the tree, then an
    /// index per shared group under it.
    key: (u32, [u8; 4]),
}

/// A file the scene names beside itself and reads once: the boxes its lights are clipped against,
/// how much of the sky reaches each of its parts, and the game's own textures its shaders read.
enum Aside {
    Wanted(String),
    Fetching(String, TrackedPromise<Result<Vec<u8>>>),
    Done,
}

enum Texture {
    Fetching(TrackedPromise<Result<DecodedTexture>>),
    Ready(TextureHandle),
    Absent,
}

/// A texture a material reads through a sampler declared over slices. Read whole and handed to the
/// graph rather than to egui, which holds nothing but planes.
enum Stack {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready,
    Absent,
}

/// A file the scene still has to read placements out of.
enum Held {
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Parsing(Vec<u8>),
    Ready(Rc<Source>),
    Failed,
}

/// The ground, which no layer places: it is tiled from the plates a `.tera` beside the zone's
/// layer groups lists.
enum Terrain {
    Wanted(String),
    Fetching(String, TrackedPromise<Result<Vec<u8>>>),
    Done,
}

/// The grass, which no layer places either: a zone file beside the layer groups names the models
/// and sorts the grids, and a grid file per cell holds the placements themselves.
enum Grass {
    Wanted(String),
    Fetching(String, TrackedPromise<Result<Vec<u8>>>),
    Placing(Box<Placing>),
    Done,
}

struct Placing {
    directory: String,
    /// The scene's model for each grass slot, in the order the zone names them.
    models: Vec<usize>,
    /// The color map each auto layer's blades are cut out of, where the zone names one.
    maps: Vec<String>,
    grids: Vec<Patch>,
    layer: usize,
}

/// One grid's blades at one auto layer, as the scene stood them up.
struct Turf {
    origin: Vec3,
    radius: f32,
    layer: usize,
    blades: usize,
}

/// One grid of grass, which is only asked for once the eye reaches the sphere the zone sorts it by.
struct Patch {
    center: Vec3,
    radius: f32,
    file: String,
    fetch: Option<TrackedPromise<Result<Vec<u8>>>>,
    taken: bool,
}

/// A file named but not yet walked.
struct Expand {
    path: String,
    transform: Mat4,
    /// How an `.lcb` entry reaches into this subtree.
    key: (u32, [u8; 4]),
    /// The largest the transform above scales by, which the bounding spheres underneath it grow by.
    scale: f32,
    /// The layer everything found belongs to. A level names whole layer groups rather than placing
    /// anything itself, so what it names brings layers of its own.
    layer: Option<usize>,
    depth: u8,
    /// The motions this subtree hangs under, each with the fixed transform in front of it.
    chain: Vec<(usize, Mat4)>,
    /// What has accumulated since the last of them, which the walk goes on adding to.
    since: Mat4,
}

#[derive(Clone, Copy)]
struct Camera {
    position: Vec3,
    yaw: f32,
    pitch: f32,
}

impl Camera {
    fn forward(&self) -> Vec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(cos_pitch * sin_yaw, sin_pitch, cos_pitch * cos_yaw)
    }

    fn right(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(-cos_yaw, 0.0, sin_yaw)
    }
}

pub struct Scene {
    camera: Camera,
    home: Camera,
    /// The level this view was opened for, which is what a preset's own is checked against.
    path: String,
    /// The last TitleEdit preset read, which is where a capture was taken from.
    preset: Option<preset::Preset>,
    /// A preset being picked or written, since a file dialog answers a frame or more later. Held
    /// rather than forgotten: dropping a promise cancels the future behind it.
    picking: Option<TrackedPromise<Option<Vec<u8>>>>,
    /// A preset pasted in whole, for a window nothing can open a file dialog over.
    pasted: String,
    saving: Option<TrackedPromise<()>>,
    /// Where the eye stood when the instance buffers were last written.
    written: Vec3,
    dirty: bool,
    load: f32,
    speed: f32,
    fov: f32,
    layers: Vec<Layer>,
    placements: Vec<Placement>,
    models: Vec<Model>,
    model_at: HashMap<String, usize>,
    materials: Vec<(String, Slot)>,
    /// Materials a model the wind may reach is drawn with, which is what its own header states.
    waving: HashSet<usize>,
    material_at: HashMap<String, usize>,
    packages: HashMap<String, Package>,
    /// The bytecode still owed on the packages a store served in part.
    blobs: HashMap<String, Blobs>,
    /// Materials whose own shaders have been asked for, so a package is only read again when a new
    /// one names it.
    picked: HashSet<usize>,
    /// Parameter files folded into the table the card holds, so a later one uploads it again.
    typed: usize,
    translated: HashMap<usize, Translated>,
    tables: HashMap<usize, Arc<(Vec<u16>, usize, usize)>>,
    lighting: Option<Arc<mdl::gpu::Lighting>>,
    /// The chain that works the frame's brightness out and reads it back through a curve, once its
    /// six shaders have arrived. Absent where the environment states no tone mapping of its own.
    exposure: Option<Arc<mdl::gpu::Exposure>>,
    /// The pass that fills whatever the frame did not cover, and the size and resource id of the
    /// volume it reads: a sky is addressed by its own texel centers, so the pass needs its shape.
    skybox: Option<Arc<program::Program>>,
    sunlight: Option<Arc<program::Program>>,
    moonlight: Option<Arc<program::Program>>,
    /// The pass that fades a distant pixel toward the weather's own fog and then toward that sky.
    haze: Option<Arc<program::Program>>,
    /// The two cloud draws, the band first, and the texture each reads: the weather names one per
    /// mesh by id, so moving the hour or the weather fetches the next.
    clouds: [Option<Arc<program::Program>>; 2],
    cloud_files: [Aside; 2],
    cloud_wanted: [Option<u16>; 2],
    sky_volume: Option<(u32, (f32, f32), f32)>,
    /// The sky the volume was fetched for, so moving the picker fetches the next one.
    sky_wanted: Option<u16>,
    sky_file: Aside,
    /// The chain that spreads the bright end of the frame into a halo.
    glare: Option<Arc<mdl::gpu::Glare>>,
    /// The pair that smooths its edges, and the chain that works out how much sky reaches a pixel.
    smoothing: Option<Arc<mdl::gpu::Smoothing>>,
    occlusion: Option<Arc<mdl::gpu::Occlusion>>,
    /// The one that darkens its corners, and what the passes past the composite are run with.
    vignette: Option<Arc<program::Program>>,
    reflection: Option<Arc<mdl::deferred::Reflection>>,
    look: program::Look,
    ambient: ambient::Ambient,
    lights: Vec<Light>,
    /// The box each light is clipped against, by the key its `.lcb` entry uses.
    clips: HashMap<(u32, [u8; 4]), (Vec3, Vec3)>,
    clip: Aside,
    /// How much of the sky reaches each part, by the key its `.svb` entry uses.
    visibility: HashMap<(u32, [u8; 4]), f32>,
    sky: Aside,
    /// The engine's own textures, by resource id. The ramp every placed light reads its falloff off
    /// is wanted from the start, since the lighting passes read it whatever a zone holds; the rest
    /// are only worth their fetch once a material's own shaders turn out to declare one.
    engine: BTreeMap<u32, Aside>,
    textures: BTreeMap<String, Texture>,
    /// The same, for the ones read through a sampler with slices. Keyed by an `Arc` so a surface
    /// built every frame names one without copying the path.
    stacked: BTreeMap<Arc<str>, Stack>,
    resident: usize,
    files: HashMap<String, Held>,
    waiting: Vec<Expand>,
    terrain: Terrain,
    grass: Grass,
    /// The two readings the grass is drawn with, once its package has arrived.
    sward: Option<Arc<gpu::Grass>>,
    turf: Vec<Turf>,
    /// The quads those stand as, and the ones a grid that arrived past the cap would have added.
    blades: usize,
    unsown: usize,
    /// Placements the view was last framed over, so a scene that arrived empty frames itself once
    /// its first file lands rather than leaving the camera at the origin.
    fitted: usize,
    renderer: Arc<Mutex<gpu::Renderer>>,
    /// Where each model stands at each detail level, as the last rebuild left them.
    placed: Vec<[Vec<program::Instance>; 3]>,
    /// What the zone's shared groups animate, and how far along their timelines it stands. The unit
    /// is not named by any file; the game's own timelines run at thirty of these to the second.
    motions: Vec<Motion>,
    clock: f32,
    /// Placements the last rebuild would have drawn had their model arrived.
    absent: usize,
}

fn rotation(angles: [f32; 3]) -> Mat3 {
    Mat3::from_rotation_z(angles[2])
        * Mat3::from_rotation_y(angles[1])
        * Mat3::from_rotation_x(angles[0])
}

/// How an `.lcb` entry reaches one instance: the key of whatever stands at the top of the tree, then
/// an index per shared group under it.
fn reach(key: (u32, [u8; 4]), depth: u8, id: u32) -> (u32, [u8; 4]) {
    if depth == 0 {
        return (id, [0; 4]);
    }
    let mut held = key.1;
    if let Some(slot) = held.get_mut(usize::from(depth) - 1) {
        *slot = id as u8;
    }
    (key.0, held)
}

/// The quad one auto-layer placement stands as, measured from its grid's own origin. The blade
/// itself is stated in no file: what the grid holds is where each stands, how far it is turned, and
/// how wide and tall it is.
fn blade(placement: &ggd::Placement, into: &mut Vec<gpu::Corner>) {
    let turn = Quat::from_array(placement.rotation());
    let across = turn * Vec3::X * placement.scale_xz() * 0.5;
    let up = turn * Vec3::Y * placement.scale_y();
    let foot = Vec3::from_array(placement.position());
    let tile = 1.0 / f32::from(TILES);
    let column = f32::from(placement.profile() % TILES) * tile;
    let half = |values: [f32; 4]| values.map(f16::from_f32);
    for (side, height, u, v) in [
        (-1.0, 1.0, 0.0, 0.0),
        (1.0, 1.0, tile, 0.0),
        (-1.0, 0.0, 0.0, 1.0),
        (1.0, 0.0, tile, 1.0),
    ] {
        let at = foot + across * side + up * height;
        into.push(gpu::Corner {
            position: half([at.x, at.y, at.z, 0.0]),
            uv: half([u, v, 0.0, 0.0]),
            color1: half([column, 0.0, 0.0, 0.0]),
            // Nought weight, so the albedo is the color map's own texel: the map the tint would be
            // read off is the engine's and no file names it.
            color: [0; 4],
        });
    }
}

/// A file the scene names beside itself, wanted where it names one.
fn aside(path: Option<&String>) -> Aside {
    match path {
        Some(path) if !path.is_empty() => Aside::Wanted(path.clone()),
        _ => Aside::Done,
    }
}

fn matrix(transform: Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::from_array(transform.scale()),
        Quat::from_mat3(&rotation(transform.rotation())),
        Vec3::from_array(transform.translation()),
    )
}

/// The detail level to draw an instance at, given how much of the view it covers. A model missing
/// the level it asked for falls back to the nearest one it has.
fn level(drawn: [bool; 3], apparent: f32) -> Option<usize> {
    let wanted = usize::from(detail(apparent));
    (wanted..3)
        .chain((0..wanted).rev())
        .find(|level| drawn[*level])
}

/// The detail level something this size on screen is drawn at, before what the file holds is known.
fn detail(apparent: f32) -> u8 {
    match apparent {
        size if size > DETAIL[0] => 0,
        size if size > DETAIL[1] => 1,
        _ => 2,
    }
}

/// A point the bulk of the placements sit around, and how far out that bulk reaches, from medians
/// rather than extremes.
fn bulk(points: &[Vec3]) -> (Vec3, f32) {
    if points.is_empty() {
        return (Vec3::ZERO, 100.0);
    }
    let order = |a: &f32, b: &f32| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
    let median = |axis: fn(&Vec3) -> f32| {
        let mut values: Vec<f32> = points.iter().map(axis).collect();
        values.sort_by(order);
        values[values.len() / 2]
    };
    let center = Vec3::new(median(|at| at.x), median(|at| at.y), median(|at| at.z));
    let mut spans: Vec<f32> = points.iter().map(|at| (*at - center).length()).collect();
    spans.sort_by(order);
    let reach = spans[((spans.len() - 1) as f32 * BULK) as usize].max(10.0);
    (center, reach)
}

fn looking_at(center: Vec3, reach: f32) -> Camera {
    let back = reach * MARGIN;
    let position = center + Vec3::new(0.0, back * 0.45, -back);
    let to = center - position;
    Camera {
        position,
        yaw: to.x.atan2(to.z),
        pitch: to.y.atan2((to.x * to.x + to.z * to.z).sqrt()),
    }
}

impl Scene {
    pub(super) fn new(path: &str, source: &Source) -> Self {
        // Both files a zone is entered through sit in its `level` directory, and its ground sits in
        // `bgplate` beside that. A shared group states a zone root of its own but has no ground.
        let root = path.split_once("/level/").map(|(root, _)| root);
        let home = looking_at(Vec3::ZERO, 100.0);
        let mut scene = Self {
            camera: home,
            home,
            path: path.to_owned(),
            preset: preset::taken(path),
            picking: None,
            pasted: String::new(),
            saving: None,
            written: Vec3::splat(f32::INFINITY),
            dirty: true,
            load: LOADED,
            speed: 1.0,
            fov: FOV.to_degrees(),
            layers: Vec::new(),
            placements: Vec::new(),
            models: Vec::new(),
            model_at: HashMap::new(),
            materials: Vec::new(),
            waving: HashSet::new(),
            material_at: HashMap::new(),
            packages: HashMap::new(),
            blobs: HashMap::new(),
            picked: HashSet::new(),
            typed: 0,
            translated: HashMap::new(),
            tables: HashMap::new(),
            lighting: None,
            exposure: None,
            skybox: None,
            sunlight: None,
            moonlight: None,
            haze: None,
            clouds: [None, None],
            cloud_files: [Aside::Done, Aside::Done],
            cloud_wanted: [None, None],
            sky_volume: None,
            sky_wanted: None,
            sky_file: Aside::Done,
            glare: None,
            smoothing: None,
            occlusion: None,
            vignette: None,
            reflection: None,
            look: program::Look::default(),
            ambient: ambient::Ambient::new(source.scene()),
            lights: Vec::new(),
            clips: HashMap::new(),
            clip: aside(source.scene().map(layer::Scene::light_culling_path)),
            visibility: HashMap::new(),
            sky: aside(source.scene().map(layer::Scene::sky_visibility_path)),
            engine: BTreeMap::from([(
                mdl::deferred::RAMP.0,
                Aside::Wanted(mdl::deferred::RAMP.1.to_owned()),
            )]),
            textures: BTreeMap::new(),
            stacked: BTreeMap::new(),
            resident: 0,
            files: HashMap::new(),
            waiting: Vec::new(),
            terrain: match root {
                Some(root) => Terrain::Wanted(format!("{root}/bgplate/terrain.tera")),
                None => Terrain::Done,
            },
            grass: match root {
                Some(root) => Grass::Wanted(format!("{root}/grass/grass_zone_data.gzd")),
                None => Grass::Done,
            },
            sward: None,
            turf: Vec::new(),
            blades: 0,
            unsown: 0,
            fitted: 0,
            renderer: gpu::Renderer::new(),
            placed: Vec::new(),
            motions: Vec::new(),
            clock: 0.0,
            absent: 0,
        };
        match source.scene() {
            // A level holds no instances of its own; the layer groups it names are where the zone
            // actually is.
            Some(named) if source.groups().is_empty() => {
                for path in named.layer_group_paths() {
                    scene.waiting.push(Expand {
                        path: path.clone(),
                        transform: Mat4::IDENTITY,
                        key: (0, [0; 4]),
                        scale: 1.0,
                        layer: None,
                        depth: 0,
                        chain: Vec::new(),
                        since: Mat4::IDENTITY,
                    });
                }
            }
            _ => scene.walk(
                source.groups(),
                source.scene().map_or(&[][..], SceneTimeline::of),
                Mat4::IDENTITY,
                (0, [0; 4]),
                1.0,
                None,
                0,
                None,
                &[],
                Mat4::IDENTITY,
            ),
        }
        scene.fit();
        scene
    }

    /// Reads placements out of a file's layers, queueing every shared group it names.
    #[allow(clippy::too_many_arguments)]
    /// The motion a scene's timelines give one of its own instances, where they give it one.
    fn motion(&mut self, timelines: &[SceneTimeline], instance: u32) -> Option<usize> {
        for timeline in timelines {
            if !timeline.auto_play() {
                continue;
            }
            let Some((actor, _)) = timeline
                .animated()
                .iter()
                .find(|(_, held)| *held as u32 == instance)
            else {
                continue;
            };
            let held = timeline.timeline();
            // The scene names an actor by the key the actor itself carries, not by its item id.
            let Some(tracks) = held.items().iter().find_map(|item| match item {
                tmb::Item::Actor(held) if i32::from(held.time()) == *actor => Some(held.tracks()),
                _ => None,
            }) else {
                continue;
            };
            // Every track of the actor and every command of each, since an actor states its motion
            // across all of them: eight of the game's aetherytes hang four tracks off one actor.
            // The first to name a channel keeps it, so a lone track reads exactly as it did.
            let mut curves: Vec<(tmb::Channel, tmb::Curve)> = Vec::new();
            for track in tracks {
                let Some(commands) = held.items().iter().find_map(|item| match item {
                    tmb::Item::Track(held) if held.id() == *track => Some(held.commands()),
                    _ => None,
                }) else {
                    continue;
                };
                for command in commands {
                    let curve_id = held.items().iter().find_map(|item| match item {
                        tmb::Item::Command(held) if held.id() == *command => match held.kind() {
                            tmb::CommandKind::C013(held) => Some(held.curve_id()),
                            _ => None,
                        },
                        _ => None,
                    });
                    let Some(found) = curve_id.and_then(|curve_id| {
                        held.items().iter().find_map(|item| match item {
                            tmb::Item::Curves(held) if i32::from(held.id()) == curve_id => {
                                Some(held.curves())
                            }
                            _ => None,
                        })
                    }) else {
                        continue;
                    };
                    for curve in found {
                        if curves.iter().all(|(channel, _)| *channel != curve.channel()) {
                            curves.push((curve.channel(), curve.clone()));
                        }
                    }
                }
            }
            if curves.is_empty() {
                continue;
            }
            let duration = held
                .items()
                .iter()
                .find_map(|item| match item {
                    tmb::Item::Header(held) => Some(f32::from(held.duration())),
                    _ => None,
                })
                .unwrap_or(1.0);
            self.motions.push(Motion { curves, duration });
            return Some(self.motions.len() - 1);
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        groups: &[LayerGroup],
        timelines: &[SceneTimeline],
        transform: Mat4,
        key: (u32, [u8; 4]),
        scale: f32,
        under: Option<usize>,
        depth: u8,
        origin: Option<&str>,
        chain: &[(usize, Mat4)],
        since: Mat4,
    ) {
        for group in groups {
            for layer in group.layers() {
                let at = match under {
                    Some(at) => at,
                    None => {
                        self.layers.push(Layer {
                            name: layer.name().clone(),
                            origin: origin.map(str::to_owned),
                            visible: layer.visible(),
                            festival: layer.festival_id(),
                            shown: layer.visible() && layer.festival_id() == 0,
                            placements: 0,
                        });
                        self.layers.len() - 1
                    }
                };
                for instance in layer.instances() {
                    let placed = instance.transform();
                    // What a timeline moves stands where the curves put it rather than where the
                    // file did, and everything under it follows.
                    let moved = self.motion(timelines, instance.id());
                    let local = match moved {
                        Some(at) => self.motions[at].at(0.0),
                        None => matrix(placed),
                    };
                    let here = transform * local;
                    // A moved node joins the chain with whatever fixed transform led up to it, and
                    // what follows accumulates from there. Everything else just lengthens the tail.
                    let (chain, since) = match moved {
                        Some(at) => {
                            let mut held = chain.to_vec();
                            held.push((at, since));
                            (held, Mat4::IDENTITY)
                        }
                        None => (chain.to_vec(), since * local),
                    };
                    match instance.data() {
                        InstanceData::BgPart(part)
                            if part.visible() && !part.asset_path().is_empty() =>
                        {
                            let model = self.model(part.asset_path());
                            self.models[model].instances += 1;
                            self.layers[at].placements += 1;
                            self.placements.push(Placement {
                                model,
                                transform: here,
                                driven: (!chain.is_empty()).then(|| {
                                    Rc::new(Driven {
                                        chain: chain.clone(),
                                        tail: since,
                                    })
                                }),
                                center: here.transform_point3(Vec3::ZERO),
                                radius: part.bounding_sphere_size() * scale,
                                fade: part.fade_out_distance(),
                                layer: at,
                                key: reach(key, depth, instance.id()),
                            });
                        }
                        InstanceData::SharedGroup(shared)
                            if depth < DEPTH && !shared.asset_path().is_empty() =>
                        {
                            self.waiting.push(Expand {
                                path: shared.asset_path().clone(),
                                transform: here,
                                key: reach(key, depth, instance.id()),
                                scale: scale
                                    * Vec3::from_array(placed.scale())
                                        .abs()
                                        .max_element()
                                        .max(0.001),
                                layer: Some(at),
                                depth: depth + 1,
                                chain,
                                since,
                            });
                        }
                        InstanceData::EnvSpace(space) => {
                            self.ambient.spaces.push(ambient::Space {
                                placement: here,
                                // The composite reads the kind back with the bit pattern, not the
                                // value, so it goes in as one.
                                shape: f32::from_bits(space.shape() as u32),
                                range: space.effective_range(),
                                bound: space.bound_instance_id(),
                            });
                        }
                        InstanceData::EnvLocation(env) => {
                            self.ambient
                                .locate(instance.id(), env.ambient_light_asset_path());
                        }
                        InstanceData::Light(light) => {
                            let held = light.colour();
                            let color = Vec3::new(
                                f32::from(held.red()),
                                f32::from(held.green()),
                                f32::from(held.blue()),
                            ) / 255.0;
                            // Without the scale a parent carries: a light's own space is where the
                            // box it is clipped against is stated, so a shared group placed at
                            // eight tenths over would light a volume of a different size than the
                            // one the zone cut for it.
                            let (_, turn, at) = here.to_scale_rotation_translation();
                            let here = Mat4::from_rotation_translation(turn, at);
                            self.lights.push(Light {
                                placement: here,
                                center: at,
                                min: Vec3::splat(-REACH),
                                max: Vec3::splat(REACH),
                                range: light.range(),
                                color: color * held.intensity(),
                                kind: match light.kind() {
                                    LightKind::Spot => program::LampKind::Spot,
                                    LightKind::Line => program::LampKind::Line,
                                    LightKind::Flat => program::LampKind::Plane,
                                    _ => program::LampKind::Point,
                                },
                                direction: here.transform_vector3(Vec3::Z).normalize_or_zero(),
                                // The two angles the file states together and halved, which is
                                // what the box its zone clips it against is cut to.
                                cone: ((light.spot_angle() + light.attenuation_cone_coefficient())
                                    * 0.5)
                                    .to_radians()
                                    .cos(),
                                key: reach(key, depth, instance.id()),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn model(&mut self, path: &str) -> usize {
        if let Some(at) = self.model_at.get(path) {
            return *at;
        }
        self.models.push(Model {
            path: path.to_owned(),
            state: State::Wanted,
            drawn: [false; 3],
            meshes: Vec::new(),
            instances: 0,
            nearest: f32::INFINITY,
            finest: 2,
            asked: 2,
        });
        self.model_at.insert(path.to_owned(), self.models.len() - 1);
        self.models.len() - 1
    }

    fn material(&mut self, path: &str) -> usize {
        if let Some(at) = self.material_at.get(path) {
            return *at;
        }
        self.materials.push((path.to_owned(), Slot::Wanted));
        self.material_at
            .insert(path.to_owned(), self.materials.len() - 1);
        self.materials.len() - 1
    }

    /// Puts the camera where the placements read so far are.
    fn fit(&mut self) {
        let points: Vec<Vec3> = self
            .placements
            .iter()
            .map(|placement| placement.center)
            .collect();
        let (center, reach) = bulk(&points);
        self.home = looking_at(center, reach);
        self.camera = self.home;
        self.fitted = points.len();
        self.dirty = true;
    }

    /// The ground, as one placement per plate. Meddle places a plate at the position the terrain
    /// file states with no rotation of its own, which is what this does.
    fn place_terrain(&mut self, path: &str, bytes: Vec<u8>) {
        let terrain = match tera::Terrain::read(Cursor::new(bytes)) {
            Ok(terrain) => terrain,
            Err(why) => {
                log::error!("assets/layer: {path}: {why}");
                return;
            }
        };
        let directory = path.trim_end_matches("terrain.tera");
        self.layers.push(Layer {
            name: "terrain".to_owned(),
            origin: Some(path.to_owned()),
            visible: true,
            festival: 0,
            shown: true,
            placements: terrain.plates().len(),
        });
        let at = self.layers.len() - 1;
        for (index, plate) in terrain.plates().iter().enumerate() {
            let (x, z) = terrain.plate_position(*plate);
            let model = self.model(&format!("{directory}{}", tera::Terrain::plate_file(index)));
            self.models[model].instances += 1;
            let center = Vec3::new(x, 0.0, z);
            self.placements.push(Placement {
                model,
                transform: Mat4::from_translation(center),
                driven: None,
                center,
                radius: PLATE,
                fade: 0.0,
                layer: at,
                key: (0, [0; 4]),
            });
        }
        self.dirty = true;
    }

    fn load_terrain(&mut self, backend: &Backend) {
        let mut arrived = None;
        let next = match &self.terrain {
            Terrain::Wanted(path) => {
                let files = backend.files().clone();
                let wanted = path.clone();
                Some(Terrain::Fetching(
                    path.clone(),
                    TrackedPromise::spawn_local(async move { files.read(&wanted).await }),
                ))
            }
            Terrain::Fetching(path, promise) => match promise.try_get() {
                Some(Ok(bytes)) => {
                    arrived = Some((path.clone(), bytes.clone()));
                    Some(Terrain::Done)
                }
                // Plenty of zones are interiors with no ground of their own.
                Some(Err(_)) => Some(Terrain::Done),
                None => None,
            },
            Terrain::Done => None,
        };
        if let Some(next) = next {
            self.terrain = next;
        }
        if let Some((path, bytes)) = arrived {
            self.place_terrain(&path, bytes);
        }
    }

    /// The zone's grass file, which names the models and sorts the grids but places nothing itself.
    fn open_grass(&mut self, path: &str, bytes: Vec<u8>) {
        let zone = match gzd::GrassZone::read(Cursor::new(bytes)) {
            Ok(zone) => zone,
            Err(why) => {
                log::error!("assets/layer: {path}: {why}");
                return;
            }
        };
        let directory = path.trim_end_matches("grass_zone_data.gzd").to_owned();
        // The zone names its models by full path, and shares them across zones: an s1f2 grid places
        // s1f1's plants.
        let models = zone
            .model_paths()
            .iter()
            .map(|path| self.model(path))
            .collect();
        let grids: Vec<Patch> = [gzd::Detail::High, gzd::Detail::Medium, gzd::Detail::Low]
            .into_iter()
            .flat_map(|detail| zone.grids(detail))
            .map(|grid| Patch {
                center: Vec3::from_array(grid.center()),
                radius: grid.radius(),
                file: grid.file(),
                fetch: None,
                taken: false,
            })
            .collect();
        self.layers.push(Layer {
            name: "grass".to_owned(),
            origin: Some(path.to_owned()),
            visible: true,
            festival: 0,
            shown: true,
            placements: 0,
        });
        let maps = zone
            .color_map()
            .iter()
            .map(|name| match name.is_empty() {
                true => String::new(),
                false => format!("{directory}{name}.tex"),
            })
            .collect();
        self.grass = Grass::Placing(Box::new(Placing {
            directory,
            models,
            maps,
            grids,
            layer: self.layers.len() - 1,
        }));
    }

    /// Every placement of one grid: the leading count slots are the procedural layers, whose
    /// placements stand as blades of the zone's own grass, and the rest name a model the zone lists.
    fn place_grass(&mut self, grid: usize, bytes: Vec<u8>) {
        let Grass::Placing(placing) = &self.grass else {
            return;
        };
        let (models, layer) = (placing.models.clone(), placing.layer);
        let radius = placing.grids[grid].radius;
        let file = match ggd::GrassGrid::read(Cursor::new(bytes)) {
            Ok(file) => file,
            Err(why) => {
                log::error!("assets/layer: grass grid {grid}: {why}");
                return;
            }
        };
        let origin = Vec3::from_array(file.world_origin());
        let mut sown: [Vec<gpu::Corner>; ggd::Chunk::AUTO_LAYERS] = Default::default();
        for chunk in file.chunks() {
            let mut at = 0;
            for (slot, count) in chunk.counts().iter().enumerate() {
                let placements = &chunk.placements()[at..at + usize::from(*count)];
                at += usize::from(*count);
                let Some(model) = slot.checked_sub(ggd::Chunk::AUTO_LAYERS) else {
                    for placement in placements {
                        blade(placement, &mut sown[slot]);
                    }
                    continue;
                };
                let Some(model) = models.get(model).copied() else {
                    continue;
                };
                for placement in placements {
                    let scale = Vec3::new(
                        placement.scale_xz(),
                        placement.scale_y(),
                        placement.scale_xz(),
                    );
                    let center = origin + Vec3::from_array(placement.position());
                    self.models[model].instances += 1;
                    self.layers[layer].placements += 1;
                    self.placements.push(Placement {
                        model,
                        driven: None,
                        transform: Mat4::from_scale_rotation_translation(
                            scale,
                            Quat::from_array(placement.rotation()),
                            center,
                        ),
                        center,
                        radius: scale.max_element(),
                        fade: 0.0,
                        layer,
                        key: (0, [0; 4]),
                    });
                }
            }
        }
        self.sow(origin, radius, sown);
        self.dirty = true;
    }

    /// One grid's blades handed to the card, a buffer per auto layer. Past the cap a whole grid is
    /// left out rather than half of one, and how many that came to is what the panel reports.
    fn sow(&mut self, origin: Vec3, radius: f32, sown: [Vec<gpu::Corner>; ggd::Chunk::AUTO_LAYERS]) {
        let Grass::Placing(placing) = &self.grass else {
            return;
        };
        let cut: [bool; ggd::Chunk::AUTO_LAYERS] =
            std::array::from_fn(|at| placing.maps.get(at).is_some_and(|path| !path.is_empty()));
        for (layer, corners) in sown.into_iter().enumerate() {
            let blades = corners.len() / 4;
            // A layer the zone names no map for is cut out of nothing, so it stands nothing up.
            if blades == 0 || !cut[layer] {
                continue;
            }
            if self.blades + blades > BLADES {
                self.unsown += blades;
                continue;
            }
            let indices = (0..blades as u32)
                .flat_map(|at| [0, 1, 2, 2, 1, 3].map(|corner| at * 4 + corner))
                .collect();
            self.renderer.lock().unwrap().queue_turf(gpu::Sown {
                turf: self.turf.len(),
                corners,
                indices,
            });
            self.turf.push(Turf {
                origin,
                radius,
                layer,
                blades,
            });
            self.blades += blades;
        }
    }

    fn load_grass(&mut self, backend: &Backend) {
        let mut arrived = None;
        let next = match &self.grass {
            Grass::Wanted(path) => {
                let files = backend.files().clone();
                let wanted = path.clone();
                Some(Grass::Fetching(
                    path.clone(),
                    TrackedPromise::spawn_local(async move { files.read(&wanted).await }),
                ))
            }
            Grass::Fetching(path, promise) => match promise.try_get() {
                Some(Ok(bytes)) => {
                    arrived = Some((path.clone(), bytes.clone()));
                    None
                }
                // Interiors and instanced zones place no grass of their own.
                Some(Err(_)) => Some(Grass::Done),
                None => None,
            },
            Grass::Placing(_) | Grass::Done => None,
        };
        if let Some(next) = next {
            self.grass = next;
        }
        if let Some((path, bytes)) = arrived {
            self.open_grass(&path, bytes);
        }
        self.load_grids(backend);
    }

    /// Asks for the grids the eye has reached, a few at a time, and places each as it lands.
    fn load_grids(&mut self, backend: &Backend) {
        let (eye, load) = (self.camera.position, self.load);
        let mut arrived = Vec::new();
        let Grass::Placing(placing) = &mut self.grass else {
            return;
        };
        let mut flight = placing
            .grids
            .iter()
            .filter(|grid| grid.fetch.is_some())
            .count();
        for (at, grid) in placing.grids.iter_mut().enumerate() {
            let landed = match &grid.fetch {
                Some(promise) => match promise.try_get() {
                    Some(Ok(bytes)) => {
                        arrived.push((at, bytes.clone()));
                        true
                    }
                    Some(Err(_)) => true,
                    None => false,
                },
                None => false,
            };
            if landed {
                grid.fetch = None;
                flight -= 1;
            }
        }
        // Nearest first, rather than in the order the zone lists them. A zone holds the same ground
        // three times over at three levels of detail, and the models sit in the coarsest of them:
        // taken in order, six hundred grids of nothing but procedural layers are read before the
        // first grid that places anything at all.
        while flight < GRIDS {
            let wanted = placing
                .grids
                .iter()
                .enumerate()
                .filter(|(_, grid)| {
                    !grid.taken && eye.distance(grid.center) < load + grid.radius
                })
                .min_by(|(_, one), (_, two)| {
                    eye.distance(one.center).total_cmp(&eye.distance(two.center))
                })
                .map(|(at, _)| at);
            let Some(at) = wanted else { break };
            let grid = &mut placing.grids[at];
            let files = backend.files().clone();
            let path = format!("{}{}", placing.directory, grid.file);
            grid.fetch = Some(TrackedPromise::spawn_local(
                async move { files.read(&path).await },
            ));
            grid.taken = true;
            flight += 1;
        }
        for (grid, bytes) in arrived {
            self.place_grass(grid, bytes);
        }
    }

    /// Where every model stands for where the eye now is. The transforms go to the card each frame
    /// rather than here, since a record carries the object into view space and the camera turns.
    fn rebuild(&mut self) {
        let eye = self.camera.position;
        let mut placed: Vec<[Vec<program::Instance>; 3]> = (0..self.models.len())
            .map(|_| std::array::from_fn(|_| Vec::new()))
            .collect();
        for model in &mut self.models {
            model.nearest = f32::INFINITY;
            model.finest = 2;
        }
        self.absent = 0;

        for at in 0..self.placements.len() {
            let placement = self.placements[at].clone();
            if !self.layers[placement.layer].shown {
                continue;
            }
            let span = (placement.center - eye).length() - placement.radius;
            if span > self.load || (placement.fade > 0.0 && span > placement.fade) {
                continue;
            }
            let apparent = placement.radius / span.max(0.01);
            let model = &mut self.models[placement.model];
            model.nearest = model.nearest.min(span);
            model.finest = model.finest.min(detail(apparent));
            let Some(level) = level(model.drawn, apparent) else {
                if !matches!(model.state, State::Ready | State::Failed) {
                    self.absent += 1;
                }
                continue;
            };
            placed[placement.model][level].push(program::Instance {
                transform: match &placement.driven {
                    Some(held) => held.chain.iter().fold(Mat4::IDENTITY, |into, (at, fixed)| {
                        into * *fixed * self.motions[*at].at(self.clock)
                    }) * held.tail,
                    None => placement.transform,
                },
                sky_visibility: self.visibility.get(&placement.key).copied().unwrap_or(1.0),
            });
        }
        self.placed = placed;
        self.written = eye;
        self.dirty = false;
    }

    /// The lights the frame draws, nearest first. Each is clipped against the box its zone states
    /// for it, in the light's own space.
    ///
    /// Nearest by how close a light's own volume comes, not by where its middle stands: a hall's far
    /// lamps cover more of the frame than a near one with a foot of reach, and an interior states
    /// hundreds, so a cap taken on the middles alone leaves whole galleries lit by nothing.
    fn lamps(&self) -> Vec<program::Lamp> {
        let eye = self.camera.position;
        let mut near: Vec<(f32, &Light)> = self
            .lights
            .iter()
            .map(|light| {
                let reach = light.min.abs().max(light.max.abs()).max_element();
                (((light.center - eye).length() - reach).max(0.0), light)
            })
            .filter(|(span, _)| *span <= self.load)
            .collect();
        near.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        near.into_iter()
            .take(LAMPS)
            .map(|(_, light)| {
                let (min, max) = self
                    .clips
                    .get(&light.key)
                    .copied()
                    .unwrap_or((light.min, light.max));
                program::Lamp {
                    placement: light.placement,
                    min,
                    max,
                    range: light.range,
                    color: light.color,
                    kind: light.kind,
                    direction: light.direction,
                    cone: light.cone,
                }
            })
            .collect()
    }

    /// The files read once beside the scene, as they arrive: the boxes its lights are clipped
    /// against, how much of the sky reaches each of its parts, and the game's own textures its
    /// shaders read.
    fn load_asides(&mut self, backend: &Backend) {
        // The volume the sky pass reads, which is named by the id rather than by the zone. Asked for
        // only once the pass itself has translated, since the pass is what says which resource the
        // volume is bound under.
        if self.skybox.is_some() && self.sky_wanted != self.ambient.sky() {
            self.sky_wanted = self.ambient.sky();
            self.sky_volume = None;
            self.sky_file = match self.ambient.sky() {
                Some(id) => Aside::Wanted(program::sky_texture(id)),
                None => Aside::Done,
            };
        }
        // The two cloud textures, each named by the weather rather than by the zone, and asked for
        // only once the draw that reads it has translated.
        if self.clouds[0].is_some() {
            let held = self.ambient.clouds();
            let wanted = [
                held.as_ref().and_then(|held| held.band),
                held.as_ref().and_then(|held| held.sheet),
            ];
            for (at, id) in wanted.into_iter().enumerate() {
                if self.cloud_wanted[at] == id {
                    continue;
                }
                self.cloud_wanted[at] = id;
                self.cloud_files[at] = match (at, id) {
                    (0, Some(id)) => Aside::Wanted(program::cloudside_texture(id)),
                    (_, Some(id)) => Aside::Wanted(program::cloud_texture(id)),
                    _ => Aside::Done,
                };
            }
        }
        for held in [&mut self.clip, &mut self.sky, &mut self.sky_file]
            .into_iter()
            .chain(&mut self.cloud_files)
            .chain(self.engine.values_mut())
        {
            *held = match std::mem::replace(held, Aside::Done) {
                Aside::Wanted(path) => {
                    let files = backend.files().clone();
                    let wanted = path.clone();
                    Aside::Fetching(
                        path,
                        TrackedPromise::spawn_local(async move { files.read(&wanted).await }),
                    )
                }
                held => held,
            };
        }
        let taken = |held: &mut Aside| match held {
            Aside::Fetching(path, promise) => match promise.try_get() {
                Some(Ok(bytes)) => {
                    let arrived = (path.clone(), bytes.clone());
                    *held = Aside::Done;
                    Some(arrived)
                }
                Some(Err(_)) => {
                    *held = Aside::Done;
                    None
                }
                None => None,
            },
            _ => None,
        };
        let clip = taken(&mut self.clip);
        let sky = taken(&mut self.sky);
        let volume = taken(&mut self.sky_file);
        let overcast: Vec<(usize, String, Vec<u8>)> = self
            .cloud_files
            .iter_mut()
            .enumerate()
            .filter_map(|(at, held)| taken(held).map(|(path, bytes)| (at, path, bytes)))
            .collect();
        let supplied: Vec<(u32, String, Vec<u8>)> = self
            .engine
            .iter_mut()
            .filter_map(|(id, held)| taken(held).map(|(path, bytes)| (*id, path, bytes)))
            .collect();

        if let Some((path, bytes)) = clip {
            match lcb::ClipBoxes::read(Cursor::new(bytes)) {
                Ok(held) => {
                    for group in held.groups() {
                        for entry in group.entries() {
                            self.clips.insert(
                                (entry.instance(), entry.members()),
                                (Vec3::from_array(entry.min()), Vec3::from_array(entry.max())),
                            );
                        }
                    }
                    self.dirty = true;
                }
                Err(why) => log::error!("assets/layer: {path}: {why}"),
            }
        }
        if let Some((path, bytes)) = sky {
            match svb::SkyVisibility::read(Cursor::new(bytes)) {
                Ok(held) => {
                    for group in held.groups() {
                        for entry in group.entries() {
                            self.visibility
                                .insert((entry.instance(), entry.members()), entry.visibility());
                        }
                    }
                    self.dirty = true;
                }
                Err(why) => log::error!("assets/layer: {path}: {why}"),
            }
        }
        if let Some((path, bytes)) = volume
            && let Some(held) = self
                .skybox
                .as_ref()
                .and_then(|held| held.textures.first())
                .map(|texture| texture.id)
        {
            // Read between its texels rather than at them: a sky is a handful of texels across a
            // whole sky, and the hour falls between two of its slices.
            match mdl::layered(&bytes, &path, glow::LINEAR) {
                Ok(decoded) => {
                    self.sky_volume = Some((
                        held,
                        (decoded.size.0 as f32, decoded.size.1 as f32),
                        decoded.layers as f32,
                    ));
                    self.renderer.lock().unwrap().queue_supplied(held, decoded);
                }
                Err(why) => log::error!("assets/layer: {path}: {why}"),
            }
        }
        for (at, path, bytes) in overcast {
            // Read between its texels: a cloud sheet is tiled over tens of thousands of units, so
            // one texel of it covers a good deal of sky.
            match mdl::layered(&bytes, &path, glow::LINEAR) {
                Ok(held) => self.renderer.lock().unwrap().queue_overcast(at, path, held),
                Err(why) => log::error!("assets/layer: {path}: {why}"),
            }
        }
        for (id, path, bytes) in supplied {
            let Some((_, _, filter)) = mdl::deferred::ENGINE
                .into_iter()
                .find(|(held, _, _)| *held == id)
            else {
                continue;
            };
            match mdl::layered(&bytes, &path, filter) {
                Ok(held) => self.renderer.lock().unwrap().queue_supplied(id, held),
                Err(why) => log::error!("assets/layer: {path}: {why}"),
            }
        }
    }

    /// Asks for whatever the scene still needs and takes in whatever arrived. Runs every frame.
    fn poll(&mut self, ui: &egui::Ui, backend: &Backend) {
        self.load_terrain(backend);
        self.load_grass(backend);
        self.load_asides(backend);
        self.ambient.poll(backend);
        self.expand(backend);
        if self.fitted == 0 && !self.placements.is_empty() {
            self.fit();
        }
        self.load_models(backend);
        self.load_materials(backend);
        self.load_packages(backend);
        self.load_textures(ui, backend);
        self.translate();

        // Parsing, decoding and uploading are all spread over frames, and a promise only asks for
        // repaints while it is still in flight. Without this the last of a load stalls half drawn
        // until something else happens to redraw the browser.
        if !self.waiting.is_empty()
            || self.ambient.pending()
            || self.renderer.lock().unwrap().pending() > 0
            || self
                .files
                .values()
                .any(|held| matches!(held, Held::Parsing(_)))
            || self
                .models
                .iter()
                .any(|model| matches!(model.state, State::Decoding(..)))
        {
            ui.ctx().request_repaint();
        }
    }

    /// Drives the files the scene is still reading placements out of.
    fn expand(&mut self, backend: &Backend) {
        let mut fetching = self
            .files
            .values()
            .filter(|held| matches!(held, Held::Fetching(_)))
            .count();
        for expand in &self.waiting {
            if fetching >= FILES {
                break;
            }
            if self.files.contains_key(&expand.path) {
                continue;
            }
            let files = backend.files().clone();
            let wanted = expand.path.clone();
            self.files.insert(
                expand.path.clone(),
                Held::Fetching(TrackedPromise::spawn_local(async move {
                    files.read(&wanted).await
                })),
            );
            fetching += 1;
        }

        for (path, held) in &mut self.files {
            let Held::Fetching(promise) = held else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            *held = match result {
                Ok(bytes) => Held::Parsing(bytes.clone()),
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Held::Failed
                }
            };
        }

        let parsing: Vec<String> = self
            .files
            .iter()
            .filter(|(_, held)| matches!(held, Held::Parsing(_)))
            .map(|(path, _)| path.clone())
            .take(PARSES)
            .collect();
        for path in parsing {
            let Some(Held::Parsing(bytes)) = self.files.remove(&path) else {
                continue;
            };
            let read = Cursor::new(bytes);
            let parsed = match path.ends_with(".sgb") {
                true => SharedGroupFile::read(read).map(Source::Shared),
                false => LayerGroupFile::read(read).map(Source::Group),
            };
            let held = match parsed {
                Ok(source) => Held::Ready(Rc::new(source)),
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Held::Failed
                }
            };
            self.files.insert(path, held);
        }

        let mut waiting = std::mem::take(&mut self.waiting);
        let mut ready = Vec::new();
        waiting.retain(|expand| match self.files.get(&expand.path) {
            Some(Held::Ready(source)) => {
                ready.push((
                    source.clone(),
                    expand.transform,
                    expand.key,
                    expand.scale,
                    expand.layer,
                    expand.depth,
                    expand.chain.clone(),
                    expand.since,
                ));
                false
            }
            // A file that would not arrive takes its subtree with it rather than being asked for
            // again every frame.
            Some(Held::Failed) => false,
            _ => true,
        });
        self.waiting = waiting;
        for (source, transform, key, scale, layer, depth, chain, since) in ready {
            self.walk(
                source.groups(),
                source.scene().map_or(&[][..], SceneTimeline::of),
                transform,
                key,
                scale,
                layer,
                depth,
                None,
                &chain,
                since,
            );
        }
    }

    fn load_models(&mut self, backend: &Backend) {
        let fetching = self
            .models
            .iter()
            .filter(|model| matches!(model.state, State::Fetching(_)))
            .count();
        if fetching < MODELS {
            let mut wanted: Vec<usize> = (0..self.models.len())
                .filter(|at| {
                    let model = &self.models[*at];
                    model.nearest <= self.load
                        && match model.state {
                            State::Wanted => true,
                            State::Ready => model.finest < model.asked,
                            _ => false,
                        }
                })
                .collect();
            wanted.sort_by(|a, b| {
                self.models[*a]
                    .nearest
                    .partial_cmp(&self.models[*b].nearest)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for at in wanted.into_iter().take(MODELS - fetching) {
                let files = backend.files().clone();
                let path = self.models[at].path.clone();
                let lod = self.models[at].finest;
                self.models[at].asked = lod;
                self.models[at].state = State::Fetching(TrackedPromise::spawn_local(async move {
                    files.read_model(&path, lod).await
                }));
            }
        }

        for model in &mut self.models {
            let State::Fetching(promise) = &model.state else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            model.state = match result {
                Ok((bytes, level)) => State::Decoding(bytes.clone(), *level),
                Err(why) => {
                    log::error!("assets/layer: {}: {why}", model.path);
                    State::Failed
                }
            };
        }

        let decoding: Vec<usize> = (0..self.models.len())
            .filter(|at| matches!(self.models[*at].state, State::Decoding(..)))
            .take(DECODES)
            .collect();
        for at in decoding {
            let State::Decoding(bytes, level) =
                std::mem::replace(&mut self.models[at].state, State::Failed)
            else {
                continue;
            };
            match self.decode(at, bytes, level) {
                Ok(()) => {
                    self.models[at].state = State::Ready;
                    self.dirty = true;
                }
                Err(why) => log::error!("assets/layer: {}: {why}", self.models[at].path),
            }
        }
    }

    /// Reads one detail level of a model and hands its geometry to the card.
    fn decode(&mut self, at: usize, bytes: Vec<u8>, level: u8) -> Result<()> {
        let container = ModelContainer::read(Cursor::new(bytes))?;
        let model = container.model(mdl::detail(level));
        let mut built = Vec::new();
        let mut used = Vec::new();
        for mesh in model.meshes() {
            if !mdl::draws(&mesh) {
                continue;
            }
            let (Ok(attributes), Ok(indices)) = (mesh.attributes(), mesh.indices()) else {
                continue;
            };
            let Ok(geometry) = mdl::build(&attributes, indices) else {
                continue;
            };
            let name = mesh.material().unwrap_or_default();
            let resolved = mdl::material::path(&name, 0, None).unwrap_or(name);
            used.push(self.material(&resolved));
            built.push(geometry);
        }
        if model.waving() {
            self.waving.extend(&used);
        }

        let level = usize::from(level);
        // A model may carry no standard mesh at the level it was read at, which plenty of terrain
        // plates do not. That is what the model holds rather than a failure to read it, and `drawn`
        // already says so.
        let mut drawn = [false; 3];
        drawn[level] = !built.is_empty();
        let mut levels: Vec<Vec<_>> = (0..3).map(|_| Vec::new()).collect();
        levels[level] = built;
        let mut meshes: Vec<Vec<usize>> = (0..3).map(|_| Vec::new()).collect();
        meshes[level] = used;
        self.models[at].drawn = drawn;
        self.models[at].meshes = meshes;
        self.renderer
            .lock()
            .unwrap()
            .queue_model(gpu::Pending { model: at, levels });
        Ok(())
    }

    fn load_materials(&mut self, backend: &Backend) {
        let fetching = self
            .materials
            .iter()
            .filter(|(_, slot)| matches!(slot, Slot::Fetching(_)))
            .count();
        // Only what a model that has arrived actually names, so a slot claimed by a model still
        // waiting on its own bytes costs nothing.
        let mut wanted: Vec<usize> = self
            .models
            .iter()
            .filter(|model| matches!(model.state, State::Ready) && model.nearest <= self.load)
            .flat_map(|model| model.meshes.iter().flatten().copied())
            .filter(|at| matches!(self.materials[*at].1, Slot::Wanted))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        for at in wanted.into_iter().take(MATERIALS.saturating_sub(fetching)) {
            let files = backend.files().clone();
            let path = self.materials[at].0.clone();
            self.materials[at].1 = Slot::Fetching(TrackedPromise::spawn_local(async move {
                files.read(&path).await
            }));
        }

        for (path, slot) in &mut self.materials {
            let Slot::Fetching(promise) = slot else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            *slot = match result {
                Ok(bytes) => match Material::parse(bytes) {
                    Ok(material) => Slot::Ready(Box::new(material)),
                    Err(why) => {
                        log::error!("assets/layer: {path}: {why}");
                        Slot::Failed
                    }
                },
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Slot::Failed
                }
            };
        }
    }

    /// The packages the ready materials name, plus the ones the frame is lit and resolved with.
    fn load_packages(&mut self, backend: &Backend) {
        let mut wanted: Vec<String> = [
            program::VIEW_POSITION,
            program::DIRECTIONAL,
            program::POINT,
            program::COMPOSITE,
        ]
        .map(str::to_owned)
        .to_vec();
        wanted.extend(program::PARAMETERS.map(|(_, path)| path.to_owned()));
        // Only where the environment states an exposure to run them under. A zone with no tone
        // mapping set of its own is left as the composite resolved it, so the six files it would
        // take are never asked for.
        if self.ambient.exposure(0.0).is_some() {
            wanted.extend(program::MEASURE.map(str::to_owned));
        }
        wanted.extend(program::GLARE.map(str::to_owned));
        wanted.extend(program::REFLECTION.map(str::to_owned));
        wanted.extend([
            program::FXAA_LUMA.to_owned(),
            program::FXAA.to_owned(),
            program::SKY.to_owned(),
            program::SUN.to_owned(),
            program::MOON.to_owned(),
            program::SHADOW.to_owned(),
            program::VIGNETTE.to_owned(),
        ]);
        // Only where the weather states a fog of its own, the same way the exposure chain is only
        // asked for where there is something to run it under.
        if self.ambient.fog().is_some() {
            wanted.push(program::FOG.to_owned());
        }
        if self.ambient.clouds().is_some() {
            wanted.push(program::CLOUD.to_owned());
        }
        if matches!(self.grass, Grass::Placing(_)) {
            wanted.push(program::GRASS.to_owned());
        }
        // A spot's package is twice the size of a point's and nothing can be lit with it until the
        // four above are in hand, so it is only worth a fetch of its own once they are and the zone
        // turns out to place one.
        for (kind, path) in [
            (program::LampKind::Line, program::LINE),
            (program::LampKind::Plane, program::PLANE),
        ] {
            if self.lighting.is_some() && self.lights.iter().any(|light| light.kind == kind) {
                wanted.push(path.to_owned());
            }
        }
        if self.lighting.is_some()
            && self
                .lights
                .iter()
                .any(|light| matches!(light.kind, program::LampKind::Spot))
        {
            wanted.push(program::SPOT.to_owned());
        }
        // A package the frame itself is drawn with selects off no material, so it is read whole
        // however many surfaces also name it: nothing here would know which of its blobs to ask for.
        let mut named: HashSet<String> = self
            .materials
            .iter()
            .filter_map(|(_, slot)| match slot {
                Slot::Ready(material) => Some(material.package()),
                _ => None,
            })
            .collect();
        for path in &wanted {
            named.remove(path);
        }
        wanted.extend(named.iter().cloned());
        for path in wanted {
            self.packages.entry(path).or_insert(Package::Wanted);
        }

        let mut fetching = self
            .packages
            .values()
            .filter(|held| matches!(held, Package::Fetching(_)))
            .count();
        for (path, held) in &mut self.packages {
            if fetching >= PACKAGES {
                break;
            }
            if !matches!(held, Package::Wanted) {
                continue;
            }
            let files = backend.files().clone();
            let wanted = path.clone();
            let holed = named.contains(path);
            *held = Package::Fetching(TrackedPromise::spawn_local(async move {
                match program::unnamed(&wanted) {
                    Some(hash) => Ok((
                        files.read_by_hash(program::SHADER.0, program::SHADER.1, hash, true).await?,
                        false,
                    )),
                    None if holed => files.read_package(&wanted).await,
                    None => Ok((files.read(&wanted).await?, false)),
                }
            }));
            fetching += 1;
        }

        let mut arrived = false;
        for (path, held) in &mut self.packages {
            let Package::Fetching(promise) = held else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            *held = match result {
                Ok((bytes, holed)) => {
                    if *holed
                        && let Ok(package) = ShaderPackage::parse(bytes)
                    {
                        self.blobs.insert(path.clone(), Blobs::read(&package));
                    }
                    Package::Ready(bytes.clone())
                }
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Package::Failed
                }
            };
            arrived = true;
        }
        if arrived {
            self.load_types();
        }
        self.want_blobs();
        self.load_blobs(backend);
    }

    /// Which shaders the surfaces read so far will be drawn with, asked of each package once per
    /// wave of materials rather than once per material: reading a package's tables is what costs.
    fn want_blobs(&mut self) {
        let mut fresh: HashMap<String, Vec<usize>> = HashMap::new();
        for (at, (_, slot)) in self.materials.iter().enumerate() {
            let Slot::Ready(material) = slot else {
                continue;
            };
            if self.picked.contains(&at) || !self.blobs.contains_key(&material.package()) {
                continue;
            }
            fresh.entry(material.package()).or_default().push(at);
        }
        for (path, held) in fresh {
            let Some(Package::Ready(bytes)) = self.packages.get(&path) else {
                continue;
            };
            let package = match ShaderPackage::parse(bytes) {
                Ok(package) => package,
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    continue;
                }
            };
            let Some(blobs) = self.blobs.get_mut(&path) else {
                continue;
            };
            for at in held {
                let Some((_, Slot::Ready(material))) = self.materials.get(at) else {
                    continue;
                };
                // Both readings of the wind, since a model that carries it may be read after the
                // material it shares with one that does not.
                for waving in [false, true] {
                    let mut keys = KEYS.to_vec();
                    if waving {
                        keys.push((APPLY_WAVING_ANIM, APPLY_WAVING_ANIM_ON));
                    }
                    for (pass, subview) in DRAWS {
                        let Some((vertex, pixel)) =
                            program::picks(&package, material, &keys, pass, subview)
                        else {
                            continue;
                        };
                        for shader in [vertex, pixel] {
                            if !blobs.arrived.contains(&shader)
                                && (shader as usize) < blobs.spans.len()
                            {
                                blobs.wanted.insert(shader);
                            }
                        }
                    }
                }
                self.picked.insert(at);
            }
        }
    }

    /// Asks for the bytecode a package still owes, and splices what arrives back where the file
    /// itself would have carried it.
    fn load_blobs(&mut self, backend: &Backend) {
        for (path, blobs) in &mut self.blobs {
            if let Some(promise) = blobs.fetching.take() {
                match promise.try_take() {
                    Err(promise) => blobs.fetching = Some(promise),
                    Ok(Err(why)) => {
                        log::error!("assets/layer: {path}: {why}");
                        blobs.wanted.clear();
                        self.packages.insert(path.clone(), Package::Failed);
                    }
                    Ok(Ok(filled)) => {
                        let Some(Package::Ready(bytes)) = self.packages.get_mut(path) else {
                            continue;
                        };
                        for (at, blob) in filled {
                            let span = blobs.spans[at as usize].clone();
                            if let Some(held) =
                                bytes.get_mut(span.start as usize..span.end as usize)
                            {
                                held.copy_from_slice(&blob);
                                blobs.arrived.insert(at);
                            }
                            blobs.wanted.remove(&at);
                        }
                        self.dirty = true;
                    }
                }
            }
            if blobs.fetching.is_some() || blobs.wanted.is_empty() {
                continue;
            }
            let files = backend.files().clone();
            let held = path.clone();
            let spans: Vec<(u32, std::ops::Range<u32>)> = blobs
                .wanted
                .iter()
                .map(|at| (*at, blobs.spans[*at as usize].clone()))
                .collect();
            blobs.fetching = Some(TrackedPromise::spawn_local(async move {
                futures_util::future::try_join_all(spans.into_iter().map(|(at, span)| {
                    let files = files.clone();
                    let held = held.clone();
                    async move { Ok((at, files.read_span(&held, span).await?)) }
                }))
                .await
            }));
        }
    }

    /// The table the shading passes index, from the parameter files that have arrived. A zone's
    /// surfaces name the background family's profiles, and the frame stands in with a table of
    /// nought until the file holding them lands.
    fn load_types(&mut self) {
        let files: Vec<(usize, ShaderParameters)> = program::PARAMETERS
            .iter()
            .filter_map(|(base, path)| {
                let Some(Package::Ready(bytes)) = self.packages.get(*path) else {
                    return None;
                };
                match ShaderParameters::read(Cursor::new(bytes.clone())) {
                    Ok(file) => Some((*base, file)),
                    Err(why) => {
                        log::error!("assets/layer: {path}: {why}");
                        None
                    }
                }
            })
            .collect();
        if files.len() == self.typed {
            return;
        }
        self.typed = files.len();
        let held: Vec<(usize, &ShaderParameters)> =
            files.iter().map(|(base, file)| (*base, file)).collect();
        self.renderer
            .lock()
            .unwrap()
            .queue_types(program::shader_types(&held));
    }

    /// One of the packages the frame itself is drawn with, translated where it has arrived.
    fn screen(
        &self,
        path: &str,
        pass: program::Pass,
        attachments: usize,
        keys: &[(u32, u32)],
    ) -> Option<Arc<program::Program>> {
        let Some(Package::Ready(bytes)) = self.packages.get(path) else {
            return None;
        };
        program::Program::screen(bytes, pass, attachments, keys)
            .inspect_err(|why| log::warn!("assets/layer: {path}: {why}"))
            .ok()
            .map(Arc::new)
    }

    /// One member of the post chain, translated where its file has arrived.
    fn effect(&self, path: &str, vertex: &str) -> Option<Arc<program::Program>> {
        let Some(Package::Ready(bytes)) = self.packages.get(path) else {
            return None;
        };
        program::Program::posteffect(path, bytes, vertex)
            .inspect_err(|why| log::warn!("assets/layer: {path}: {why}"))
            .ok()
            .map(Arc::new)
    }

    /// The chain that reflects the frame off itself, translated once its nine shaders have arrived.
    /// Every member is drawn with the vertex shader the game pairs it with.
    fn mirror(&self) -> Option<Arc<mdl::deferred::Reflection>> {
        let ready = |path: &str| match self.packages.get(path) {
            Some(Package::Ready(bytes)) => Some(bytes),
            _ => None,
        };
        let held = |path: &str, vertex: &str| {
            program::Program::sampling(path, ready(path)?, ready(vertex)?)
                .inspect_err(|why| log::warn!("assets/layer: {path}: {why}"))
                .ok()
                .map(Arc::new)
        };
        let read = |path: &str| held(path, program::REFLECTION_VERTEX);
        Some(Arc::new(mdl::deferred::Reflection {
            normal: read(program::REFLECTION_NORMAL)?,
            mask: read(program::REFLECTION_MASK)?,
            march: read(program::REFLECTION_MARCH)?,
            blur: [
                read(program::REFLECTION_BLUR_X)?,
                read(program::REFLECTION_BLUR_Y)?,
            ],
            distort: read(program::REFLECTION_DISTORT)?,
            copy: held(program::REFLECTION_COPY, program::REFLECTION_MERGE_VERTEX)?,
        }))
    }

    /// The pair that smooths the frame's edges, and the three that work out how much sky reaches
    /// each pixel, each translated once all of its own shaders have arrived.
    fn edges(&self) -> Option<Arc<mdl::gpu::Smoothing>> {
        Some(Arc::new(mdl::gpu::Smoothing {
            luma: self.effect(program::FXAA_LUMA, program::POST_VERTEX)?,
            fxaa: self.effect(program::FXAA, program::POST_VERTEX)?,
        }))
    }

    /// The chain that spreads the bright end of the frame, translated once its four shaders have
    /// arrived. The blur reads seven coordinates rather than one, so it is drawn with the vertex
    /// shader the game pairs it with rather than the one every other pass here takes.
    fn halo(&self) -> Option<Arc<mdl::gpu::Glare>> {
        let Some(Package::Ready(vertex)) = self.packages.get(program::SAMPLING_7) else {
            return None;
        };
        let Some(Package::Ready(bytes)) = self.packages.get(program::BLOOM_BLUR) else {
            return None;
        };
        let blur = program::Program::sampling(program::BLOOM_BLUR, bytes, vertex)
            .inspect_err(|why| log::warn!("assets/layer: {}: {why}", program::BLOOM_BLUR))
            .ok()?;
        Some(Arc::new(mdl::gpu::Glare {
            bright: self.effect(program::BRIGHT_PASS, program::POST_VERTEX)?,
            blur: Arc::new(blur),
            merge: self.effect(program::GLARE_MERGE, program::POST_VERTEX)?,
        }))
    }

    /// Withheld until its thresholds mean something here. The occlusion pass takes the distance past
    /// which two samples stop being one surface as a fraction of the depth the frame spans, and the
    /// fractions are the model viewer's, where a frame spans one model. A zone spans thousands of
    /// units, so the same fraction is hundreds of them: every tap would read as the same surface. No
    /// file states the pass's own constants, so there is nothing to scale them by yet.
    fn occluders(&self) -> Option<Arc<mdl::gpu::Occlusion>> {
        None
    }

    /// The exposure chain, translated once all six of its shaders have arrived. The three that
    /// halve the frame read four texels of a square rather than one, so they are drawn with the
    /// vertex shader that names those four.
    fn measure(&self) -> Option<Arc<mdl::gpu::Exposure>> {
        let held = |path: &str, vertex| self.effect(path, vertex);
        Some(Arc::new(mdl::gpu::Exposure {
            initial: held(program::MEASURE_INITIAL, program::SAMPLING_VERTEX)?,
            iterative: held(program::MEASURE_ITERATIVE, program::SAMPLING_VERTEX)?,
            last: held(program::MEASURE_FINAL, program::SAMPLING_VERTEX)?,
            adapt: held(program::ADAPT_LUM, program::POST_VERTEX)?,
            curve: held(program::TONE_MAP_LUT, program::POST_VERTEX)?,
            tone: held(program::TONE_MAPPING, program::POST_VERTEX)?,
        }))
    }

    /// Every ready material's shaders, translated once its package has arrived. A context that
    /// turned out to write fewer of the G-buffer's targets at once has them translated again.
    fn translate(&mut self) {
        let attachments = self.renderer.lock().unwrap().attachments();
        if self.lighting.is_none()
            && let (Some(position), Some(directional), Some(point), Some(composite)) = (
                self.screen(program::VIEW_POSITION, program::Pass::Lighting, attachments, &[]),
                self.screen(program::DIRECTIONAL, program::Pass::Lighting, attachments, &[]),
                self.screen(program::POINT, program::Pass::Lamp, attachments, &[]),
                self.screen(program::COMPOSITE, program::Pass::Composite, attachments, &[]),
            )
        {
            self.lighting = Some(Arc::new(mdl::gpu::Lighting {
                position,
                directional,
                point,
                spot: None,
                shadow: None,
                line: None,
                plane: None,
                subsurface: None,
                // The fifth target's alpha is a background surface's emissive flag rather than the
                // scale a strand is marched along, so the fur pass has nothing here to read.
                fur: None,
                composite,
            }));
        }
        // The frame lights without a spot's own package and takes it up on whichever frame it
        // arrives on: waiting for it would leave a zone unlit until it did. A package that arrived
        // and would not translate is marked failed rather than translated again every frame.
        if let Some(lighting) = self.lighting.clone()
            && lighting.spot.is_none()
            && matches!(self.packages.get(program::SPOT), Some(Package::Ready(_)))
        {
            let spot = self.screen(program::SPOT, program::Pass::Lamp, attachments, &[]);
            if spot.is_none() {
                self.packages
                    .insert(program::SPOT.to_owned(), Package::Failed);
            }
            self.lighting = Some(Arc::new(mdl::gpu::Lighting {
                spot,
                ..(*lighting).clone()
            }));
        }
        for (path, take) in [
            (program::LINE, 0usize),
            (program::PLANE, 1usize),
        ] {
            let Some(lighting) = self.lighting.clone() else {
                continue;
            };
            let held = match take {
                0 => lighting.line.is_none(),
                _ => lighting.plane.is_none(),
            };
            if !held || !matches!(self.packages.get(path), Some(Package::Ready(_))) {
                continue;
            }
            let built = self.screen(path, program::Pass::Lamp, attachments, &[]);
            if built.is_none() {
                self.packages.insert(path.to_owned(), Package::Failed);
            }
            self.lighting = Some(Arc::new(match take {
                0 => mdl::gpu::Lighting {
                    line: built,
                    ..(*lighting).clone()
                },
                _ => mdl::gpu::Lighting {
                    plane: built,
                    ..(*lighting).clone()
                },
            }));
        }
        // The same, for the pass that works out how much of the sun reaches a pixel: a zone lights
        // unshadowed until its package is in hand rather than waiting on one.
        if let Some(lighting) = self.lighting.clone()
            && lighting.shadow.is_none()
            && matches!(self.packages.get(program::SHADOW), Some(Package::Ready(_)))
        {
            // Nine taps rather than one: a single comparison shows every texel of the map as a
            // step. Both keys are asked for here alone, so no other package moves with them.
            let shadow = self.screen(
                program::SHADOW,
                program::Pass::Lighting,
                attachments,
                &[
                    (program::SHADOW_SOFT, program::SHADOW_SOFT_3X3),
                    (program::TRANSFORM_PROJ, program::TRANSFORM_PROJ_PLANE_FAR),
                ],
            );
            if shadow.is_none() {
                self.packages
                    .insert(program::SHADOW.to_owned(), Package::Failed);
            }
            self.lighting = Some(Arc::new(mdl::gpu::Lighting {
                shadow,
                ..(*lighting).clone()
            }));
        }
        if self.exposure.is_none() {
            self.exposure = self.measure();
        }
        if self.sunlight.is_none() {
            self.sunlight = self.effect(program::SUN, program::POST_VERTEX);
        }
        if self.moonlight.is_none() {
            self.moonlight = self.effect(program::MOON, program::MOON_VERTEX);
        }
        if self.skybox.is_none() {
            self.skybox = self.effect(program::SKY, program::SKY_VERTEX);
        }
        if self.haze.is_none() {
            self.haze = self.effect(program::FOG, program::POST_VERTEX);
        }
        if self.clouds[0].is_none()
            && let Some(Package::Ready(bytes)) = self.packages.get(program::CLOUD)
        {
            for (at, pass) in [program::Pass::CloudBand, program::Pass::CloudSheet]
                .into_iter()
                .enumerate()
            {
                self.clouds[at] = program::Program::cloud(bytes, pass, attachments)
                    .inspect_err(|why| log::warn!("assets/layer: {}: {why}", program::CLOUD))
                    .ok()
                    .map(Arc::new);
            }
        }
        if self.sward.is_none()
            && let Some(Package::Ready(bytes)) = self.packages.get(program::GRASS)
        {
            let read = |normal, page| {
                program::Program::grass(bytes, normal, page, attachments)
                    .inspect_err(|why| log::warn!("assets/layer: {}: {why}", program::GRASS))
                    .ok()
                    .map(Arc::new)
            };
            self.sward = read(false, 0).map(|first| {
                let pages = first.outputs.len().div_ceil(attachments.max(1)).max(1);
                let mut buffer = vec![first];
                buffer.extend((1..pages).filter_map(|page| read(false, page)));
                Arc::new(gpu::Grass {
                    buffer,
                    normal: (0..pages).filter_map(|page| read(true, page)).collect(),
                })
            });
        }
        if self.glare.is_none() {
            self.glare = self.halo();
        }
        if self.reflection.is_none() {
            self.reflection = self.mirror();
        }
        if self.smoothing.is_none() {
            self.smoothing = self.edges();
        }
        if self.occlusion.is_none() {
            self.occlusion = self.occluders();
        }
        if self.vignette.is_none() {
            // Against the sky's own vertex shader, which is the one here handing a fragment where it
            // stands rather than what to read.
            self.vignette = self.effect(program::VIGNETTE, program::SKY_VERTEX);
        }

        for (at, (_, slot)) in self.materials.iter().enumerate() {
            let Slot::Ready(material) = slot else {
                continue;
            };
            if self
                .translated
                .get(&at)
                .is_some_and(|held| held.attachments == attachments)
            {
                continue;
            }
            let Some(Package::Ready(bytes)) = self.packages.get(&material.package()) else {
                continue;
            };
            // A package still owed bytecode holds nought where a shader would be, which translates
            // to a program that draws nothing rather than to an error worth reporting.
            if self
                .blobs
                .get(&material.package())
                .is_some_and(|held| !held.wanted.is_empty())
            {
                continue;
            }
            let mut keys = KEYS.to_vec();
            if self.waving.contains(&at) {
                keys.push((APPLY_WAVING_ANIM, APPLY_WAVING_ANIM_ON));
            }
            let page = |pass, page| {
                program::Program::build(
                    bytes,
                    material,
                    &keys,
                    pass,
                    program::SUB_VIEW_MAIN,
                    page,
                    attachments,
                )
            };
            // A package with no opaque pass is a surface that blends itself into the frame - water,
            // and the glass a zone places. It fills the same G-buffer through a pass of its own and
            // answers into the lit frame afterward, so which one it took has to be remembered. One
            // with neither fills none of it: a light shaft and a slab of fog are drawn over the
            // frame the lighting left and nowhere else.
            let (pass, first, opaque) = match page(program::Pass::Buffer, 0) {
                Ok(held) => (program::Pass::Buffer, Some(held), String::new()),
                Err(why) => (
                    program::Pass::Blended,
                    page(program::Pass::Blended, 0).ok(),
                    why,
                ),
            };
            let blended = pass != program::Pass::Buffer;
            let mut buffer = Vec::new();
            if let Some(first) = first {
                let pages = first.outputs.len().div_ceil(attachments.max(1)).max(1);
                buffer.push(Arc::new(first));
                buffer.extend((1..pages).filter_map(|held| page(pass, held).ok().map(Arc::new)));
            }
            // Only where the same vertex shader settled the depth. A blending surface fills the
            // buffer through a pass whose vertices are lifted by its own waves, and the depth pass
            // leaves them where the file put them: every later test against it fails.
            let depth = match blended {
                true => Err("a blending surface writes its own depth".into()),
                false => program::Program::build(
                    bytes,
                    material,
                    &keys,
                    program::Pass::Depth,
                    program::SUB_VIEW_MAIN,
                    0,
                    attachments,
                ),
            };
            // The same depth pass as the light sees it. A package that answers no shadow subview
            // casts none, which is what the flag on a placed instance says anyway.
            let shadow = program::Program::build(
                bytes,
                material,
                &keys,
                program::Pass::Depth,
                program::SUB_VIEW_SHADOW_0,
                0,
                attachments,
            );
            // What it answers into the lit frame with, which only a blending surface has.
            // Water reads the lit frame back and shades itself from it, where anything else that
            // blends is lit where it stands and an overlay carries its own colour.
            let resolve = blended
                .then(|| {
                    [
                        program::Pass::Water,
                        program::Pass::BlendedLighting,
                        program::Pass::Shaft,
                        program::Pass::Layer,
                    ]
                    .into_iter()
                    .find_map(|pass| page(pass, 0).ok())
                    .map(Arc::new)
                })
                .flatten();
            if buffer.is_empty() && resolve.is_none() {
                log::warn!("assets/layer: {}: {opaque}", material.package());
                continue;
            }
            // The engine binds these rather than the material, so nothing names them as a path;
            // what a surface's own shaders declare is what says the file is worth reading at all.
            for texture in buffer.iter().chain(&resolve).flat_map(|held| &held.textures) {
                if let Some((id, path, _)) = mdl::deferred::ENGINE
                    .iter()
                    .find(|(held, _, _)| *held == texture.id)
                {
                    self.engine
                        .entry(*id)
                        .or_insert_with(|| Aside::Wanted(path.to_string()));
                }
            }
            self.translated.insert(
                at,
                Translated {
                    attachments,
                    buffer,
                    depth: depth.ok().map(Arc::new),
                    shadow: shadow.ok().map(Arc::new),
                    resolve,
                },
            );
            if let Some((values, columns, rows)) =
                material.held().color_table().and_then(program::table)
            {
                self.tables
                    .entry(at)
                    .or_insert_with(|| Arc::new((values.to_vec(), columns, rows)));
            }
            self.dirty = true;
        }
    }

    /// The textures a material names for a sampler its package declares over slices rather than a
    /// plane: an environment cube, an array, a volume.
    fn sliced(&self) -> BTreeSet<String> {
        self.translated
            .iter()
            .filter_map(|(at, held)| match self.materials.get(*at) {
                Some((_, Slot::Ready(material))) => Some((held, material)),
                _ => None,
            })
            .flat_map(|(held, material)| {
                material.bound().filter(|(id, _)| {
                    held.buffer
                        .iter()
                        .chain(&held.depth)
                        .chain(&held.shadow)
                        .chain(&held.resolve)
                        .flat_map(|pass| &pass.textures)
                        .any(|texture| texture.id == *id && texture.kind != program::Kind::Plane)
                })
            })
            .map(|(_, path)| path.to_owned())
            .collect()
    }

    /// Every texture the ready materials name, since the game's own shaders read all of them. Held
    /// for the whole scene rather than per model, since a zone's models share theirs heavily.
    /// The color maps the zone's grass is cut out of, where it names any.
    fn maps(&self) -> Vec<String> {
        let Grass::Placing(placing) = &self.grass else {
            return Vec::new();
        };
        placing
            .maps
            .iter()
            .filter(|path| !path.is_empty())
            .cloned()
            .collect()
    }

    fn load_textures(&mut self, ui: &egui::Ui, backend: &Backend) {
        let sliced = self.sliced();
        for path in sliced
            .iter()
            .filter(|path| !self.stacked.contains_key(path.as_str()))
            .cloned()
            .collect::<Vec<_>>()
        {
            let files = backend.files().clone();
            let held = path.clone();
            self.stacked.insert(
                path.into(),
                Stack::Fetching(TrackedPromise::spawn_local(async move {
                    files.read(&held).await
                })),
            );
        }
        for (path, stack) in &mut self.stacked {
            let Stack::Fetching(promise) = stack else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            *stack = match result
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(|bytes| {
                    mdl::layered(bytes, path, glow::LINEAR).map_err(|why| why.to_string())
                }) {
                Ok(held) => {
                    self.renderer.lock().unwrap().queue_stack(path.clone(), held);
                    self.dirty = true;
                    Stack::Ready
                }
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Stack::Absent
                }
            };
        }

        let mut fetching = self
            .textures
            .values()
            .filter(|texture| matches!(texture, Texture::Fetching(_)))
            .count();
        let maps = self.maps();
        let wanted: Vec<String> = self
            .materials
            .iter()
            .enumerate()
            .filter(|(at, _)| self.translated.contains_key(at))
            .filter_map(|(_, (_, slot))| match slot {
                Slot::Ready(material) => Some(material),
                _ => None,
            })
            .flat_map(|material| material.textures())
            .cloned()
            .chain(maps.iter().cloned())
            .filter(|path| !self.textures.contains_key(path) && !sliced.contains(path))
            .collect();
        for path in wanted {
            if fetching >= TEXTURES {
                break;
            }
            if self.textures.contains_key(&path) {
                continue;
            }
            if self.resident >= TEXTURE_BUDGET {
                self.textures.insert(path, Texture::Absent);
                continue;
            }
            let files = backend.files().clone();
            let held = path.clone();
            let size = match maps.contains(&path) {
                true => GRASS_SIZE,
                false => TEXTURE_SIZE,
            };
            self.textures.insert(
                path,
                Texture::Fetching(TrackedPromise::spawn_local(async move {
                    files.read_texture(&held, Some(size)).await
                })),
            );
            fetching += 1;
        }

        let mut taken = 0;
        for (path, texture) in &mut self.textures {
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
                    taken += size[0] * size[1] * 4;
                    // Premultiplied is the one path that copies the bytes through untouched, and a
                    // diffuse map's alpha is opacity rather than something the other channels
                    // should be scaled by.
                    Texture::Ready(ui.ctx().load_texture(
                        format!("scene:{path}"),
                        egui::ColorImage::from_rgba_premultiplied(
                            size,
                            decoded.image.as_flat_samples().as_slice(),
                        ),
                        TextureOptions {
                            magnification: egui::TextureFilter::Linear,
                            minification: egui::TextureFilter::Linear,
                            wrap_mode: egui::TextureWrapMode::Repeat,
                            mipmap_mode: Some(egui::TextureFilter::Linear),
                        },
                    ))
                }
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Texture::Absent
                }
            };
        }
        self.resident += taken;
    }

    /// The viewport, and the navigation over it.
    fn viewport(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }

        if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_delta();
            self.camera.yaw -= delta.x * 0.005;
            self.camera.pitch = (self.camera.pitch - delta.y * 0.005).clamp(-1.5, 1.5);
        }
        let mut moved = Vec3::ZERO;
        if response.dragged_by(egui::PointerButton::Secondary) {
            let delta = response.drag_delta();
            moved += (self.camera.right() * delta.x + Vec3::Y * delta.y) * self.load * 0.0005;
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                moved += self.camera.forward() * scroll * self.load * 0.002;
            }
        }
        // Keys only where nothing else has taken them, so typing in the browser's own fields does
        // not fly the camera along with it.
        let flying = (response.hovered() || response.dragged())
            && ui.memory(|memory| memory.focused().is_none());
        if flying {
            let (ahead, side, up, step) = ui.input(|input| {
                let held = |key| f32::from(input.key_down(key));
                (
                    held(egui::Key::W) - held(egui::Key::S),
                    held(egui::Key::D) - held(egui::Key::A),
                    held(egui::Key::E) - held(egui::Key::Q),
                    input.stable_dt.min(0.1)
                        * match input.modifiers.shift {
                            true => 4.0,
                            false => 1.0,
                        },
                )
            });
            if ahead != 0.0 || side != 0.0 || up != 0.0 {
                moved +=
                    (self.camera.forward() * ahead + self.camera.right() * side + Vec3::Y * up)
                        * step
                        * SPEED
                        * self.speed;
                ui.ctx().request_repaint();
            }
        }
        if moved != Vec3::ZERO {
            self.camera.position += moved;
        }
        // Scaled by how fast the camera is set to move, so raising the speed does not turn a
        // rebuild every few seconds into one every frame.
        if (self.camera.position - self.written).length() > STEP * self.speed {
            self.dirty = true;
        }
        // A timeline states where its node stands rather than how far it has moved, so what a frame
        // draws follows the clock rather than the frame before it. The placements themselves are
        // already worked out; only where the moving ones stand is done again.
        if !self.motions.is_empty() {
            self.clock += ui.input(|input| input.stable_dt).min(0.25) * TICKS;
            self.dirty = true;
            ui.ctx().request_repaint();
        }
        if self.dirty {
            self.rebuild();
        }

        let eye = self.camera.position;
        let view = Mat4::look_at_rh(eye, eye + self.camera.forward(), Vec3::Y);
        let far = self.load * 1.5;
        // Capped as well as scaled: at the largest load distance a proportional near plane would
        // sit further out than the walls of an interior.
        let near = (far * 0.0002).min(0.2);
        // The game's own shaders were compiled for a clip depth running from nought to one, and the
        // backend moves what they compute into the range GL clips against.
        let projection = Mat4::perspective_rh(
            self.fov.to_radians(),
            rect.width() / rect.height(),
            near,
            far,
        );

        let mut batches = Vec::new();
        for (at, model) in self.models.iter().enumerate() {
            for level in 0..3 {
                let instances = match self.placed.get(at) {
                    Some(held) if !held[level].is_empty() => held[level].clone(),
                    _ => continue,
                };
                let Some(meshes) = model.meshes.get(level) else {
                    continue;
                };
                batches.push(gpu::Batch {
                    model: at,
                    level,
                    instances,
                    surfaces: meshes.iter().map(|slot| self.surface(*slot)).collect(),
                });
            }
        }

        let (light, color) = self.ambient.light();
        let frame = gpu::Frame {
            scene: program::Scene {
                view,
                projection,
                model: Mat4::IDENTITY,
                light,
                diffuse: color,
                specular: color,
                ambient: self.ambient.scene(),
                // How far the adaptation moves is stated per second, so it needs to know how long a
                // frame took. A frame after an idle spell is capped by the pass itself.
                exposure: self
                    .ambient
                    .exposure(ui.input(|input| input.stable_dt))
                    .unwrap_or_default(),
                fog: self.ambient.fog().unwrap_or_default(),
                cloud: self
                    .ambient
                    .clouds()
                    .map_or_else(program::Cloud::default, |held| held.scene),
                shaft: self.ambient.shafts().unwrap_or_default(),
                look: self.look,
                clock: self.clock / TICKS,
                wind: self.ambient.wind().unwrap_or(program::Wind {
                    reach: 0.0,
                    ..Default::default()
                }),
                sky: program::Sky {
                    time: self.ambient.time,
                    tilt: self.ambient.tilt,
                    size: self
                        .sky_volume
                        .map_or_else(|| program::Sky::default().size, |(_, size, _)| size),
                    depth: self
                        .sky_volume
                        .map_or_else(|| program::Sky::default().depth, |(_, _, depth)| depth),
                    moon: self.ambient.moon,
                    moonlight: self.ambient.moonlight(),
                },
                ..Default::default()
            },
            lighting: self.lighting.clone(),
            exposure: self.exposure.clone(),
            skybox: self.skybox.clone(),
            sunlight: self.sunlight.clone(),
            moonlight: self.moonlight.clone(),
            haze: self.haze.clone(),
            clouds: self.clouds.clone(),
            glare: self.glare.clone(),
            smoothing: self.smoothing.clone(),
            occlusion: self.occlusion.clone(),
            vignette: self.look.vignette.then(|| self.vignette.clone()).flatten(),
            reflection: self.look.reflect.then(|| self.reflection.clone()).flatten(),
            lamps: self.lamps(),
            batches,
            grass: self.sward.clone(),
            blades: self.sown(),
        };

        // The context is taken from the painter rather than captured: `glow::Context` is neither
        // `Send` nor `Sync` on wasm, and a callback has to be both.
        let renderer = self.renderer.clone();
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                renderer
                    .lock()
                    .unwrap()
                    .draw(painter.gl(), painter, &frame, &info);
            })),
        });
    }

    /// Every grid's blades that stand within the distance the rest of the zone is drawn over, and
    /// the color map each is cut out of.
    fn sown(&self) -> Vec<gpu::Blades> {
        let Grass::Placing(placing) = &self.grass else {
            return Vec::new();
        };
        let eye = self.camera.position;
        self.turf
            .iter()
            .enumerate()
            .filter(|(_, turf)| eye.distance(turf.origin) < self.load + turf.radius)
            .filter_map(|(at, turf)| {
                // Nothing until the map itself is in hand. A blade is cut out of its alpha, and the
                // flat stand-in an unfilled sampler answers with is opaque: the whole quad would
                // stand there as a grey sheet.
                let color_map = match self.textures.get(placing.maps.get(turf.layer)?)? {
                    Texture::Ready(handle) => handle.id(),
                    _ => return None,
                };
                Some(gpu::Blades {
                    turf: at,
                    origin: turf.origin,
                    color_map,
                })
            })
            .collect()
    }

    fn surface(&self, slot: usize) -> gpu::Surface {
        let held = self.materials.get(slot).and_then(|(_, held)| match held {
            Slot::Ready(material) => Some(material),
            _ => None,
        });
        // The graph's own store first: a sliced texture reaches egui as a plane on the frame before
        // its package is translated, and answering with that one would pin the sampler to it.
        let bind = |path: &str| match self.stacked.get_key_value(path) {
            Some((held, Stack::Ready)) => Some(mdl::gpu::Bound::Stacked(held.clone())),
            _ => match self.textures.get(path) {
                Some(Texture::Ready(handle)) => Some(mdl::gpu::Bound::Plane(handle.id())),
                _ => None,
            },
        };
        // Bare geometry until the material and its package arrive, rather than a hole where they
        // will be.
        let shaded = held
            .zip(self.translated.get(&slot))
            .map(|(material, held)| mdl::gpu::Shaded {
                buffer: held.buffer.clone(),
                depth: held.depth.clone(),
                shadow: held.shadow.clone(),
                resolve: held.resolve.clone(),
                table: self.tables.get(&slot).cloned(),
                textures: material
                    .bound()
                    .map(|(id, path)| (id, bind(path)))
                    .collect(),
            });
        gpu::Surface {
            material: slot,
            shaded,
            cull: held.is_some_and(|material| material.cull()),
            hidden: held.is_some_and(|material| !material.drawn()),
        }
    }

    /// Stands the view where a preset says a capture was taken from: the camera and what it looks
    /// at, the lens, and the weather and hour the frame was under. The level it names is left to the
    /// link beside it, since opening one builds a scene of its own and would undo all of this.
    fn stand_where(&mut self, held: &preset::Preset) {
        let (yaw, pitch) = held.angles();
        self.camera.position = held.camera;
        self.camera.yaw = yaw;
        self.camera.pitch = pitch;
        if let Some(fov) = held.fov {
            self.fov = fov;
        }
        if let Some(time) = held.time {
            self.ambient.time = time;
        }
        if let Some(id) = held.weather
            && !self.ambient.stand_in_weather(id)
        {
            log::warn!("assets/layer: this zone states no weather {id}");
        }
        self.dirty = true;
    }

    pub fn details_ui(
        &mut self,
        ui: &mut egui::Ui,
        follow: &mut Option<String>,
        deps: &mut Deps,
        backend: &Backend,
    ) {
        let mut refit = false;
        let mut changed = false;
        // A preset dropped on the window, or picked with the button below, stands this view where a
        // capture was taken from, which is what makes the two comparable at all.
        let mut arrived: Vec<Vec<u8>> = ui.ctx().input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|held| held.bytes.as_ref().map(|held| held.to_vec()))
                .collect()
        });
        if let Some(promise) = &self.picking
            && let Some(held) = promise.try_get()
        {
            arrived.extend(held.clone());
            self.picking = None;
        }
        for bytes in &arrived {
            match preset::Preset::read(bytes) {
                Ok(held) => {
                    // A preset for somewhere else opens that level instead, and is applied on the
                    // other side: opening one builds a scene of its own.
                    match held.level == self.path {
                        true => self.stand_where(&held),
                        false => {
                            *follow = Some(held.level.clone());
                            preset::hold(held);
                            return;
                        }
                    }
                    self.preset = Some(held);
                    changed = true;
                }
                Err(why) => log::warn!("assets/layer: this is no TitleEdit preset: {why}"),
            }
        }
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            section(ui, "View");
            // Pasted rather than picked, since a file dialog is the one way in that nothing outside
            // the window can drive: a headless run positions the camera through here.
            let pasted = ui.add(
                egui::TextEdit::singleline(&mut self.pasted)
                    .hint_text("paste a TitleEdit preset")
                    .desired_width(f32::INFINITY),
            );
            let mut load =
                pasted.lost_focus() && ui.input(|held| held.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if ui.button("Import preset").clicked() {
                    self.picking = Some(TrackedPromise::spawn_local(async {
                        let held = rfd::AsyncFileDialog::new()
                            .set_title("Import a TitleEdit preset")
                            .add_filter("TitleEdit preset", &["json"])
                            .pick_file()
                            .await?;
                        Some(held.read().await)
                    }));
                }
                load |= ui.button("Load pasted").clicked();
                if load {
                    match preset::Preset::read(self.pasted.as_bytes()) {
                        Ok(held) => {
                            match held.level == self.path {
                                true => self.stand_where(&held),
                                false => {
                                    *follow = Some(held.level.clone());
                                    preset::hold(held);
                                    return;
                                }
                            }
                            self.preset = Some(held);
                            changed = true;
                        }
                        Err(why) => log::warn!("assets/layer: this is no TitleEdit preset: {why}"),
                    }
                }
                if ui.button("Export preset").clicked() {
                    let held = preset::Preset::of(
                        &self.path,
                        self.camera.position,
                        self.camera.forward(),
                        self.fov,
                        self.ambient.weather_id(),
                        self.ambient.time,
                    );
                    match held.write() {
                        Ok(text) => {
                            let name = format!("TE_{}.json", held.name);
                            self.saving = Some(TrackedPromise::spawn_local(async move {
                                if let Some(file) = rfd::AsyncFileDialog::new()
                                    .set_title("Export a TitleEdit preset")
                                    .set_file_name(&name)
                                    .save_file()
                                    .await
                                    && let Err(why) = file.write(text.as_bytes()).await
                                {
                                    log::error!("assets/layer: {name}: {why}");
                                }
                            }));
                        }
                        Err(why) => log::error!("assets/layer: {why}"),
                    }
                }
            });
            if let Some(held) = &self.preset {
                ui.label(RichText::new(format!("Preset  {}", held.name)).weak());
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                if ui.button("Fit").clicked() {
                    refit = true;
                }
                ui.label(
                    RichText::new(format!(
                        "{:.0}, {:.0}, {:.0}",
                        self.camera.position.x, self.camera.position.y, self.camera.position.z
                    ))
                    .monospace()
                    .weak(),
                );
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Load distance").weak());
            changed |= ui
                .add(egui::Slider::new(&mut self.load, NEAREST..=FURTHEST).logarithmic(true))
                .changed();
            ui.label(RichText::new("Speed").weak());
            ui.add(egui::Slider::new(&mut self.speed, 0.1..=20.0).logarithmic(true));
            ui.label(RichText::new("Field of view").weak());
            changed |= ui
                .add(egui::Slider::new(&mut self.fov, 20.0..=120.0).suffix("\u{b0}"))
                .changed();
            ui.checkbox(&mut self.look.vignette, "Vignette").on_hover_text(
                "Darken the frame's corners with the game's own pass. The ellipse it spreads over \
                 follows the frame's own shape, but the two below are choices: no file states \
                 either",
            );
            ui.add_enabled_ui(self.look.vignette, |ui| {
                ui.label(RichText::new("Onset").weak());
                ui.add(egui::Slider::new(&mut self.look.onset, 0.0..=1.0))
                    .on_hover_text(
                        "How far out the darkening starts, as a squared distance with a corner at \
                         one",
                    );
                ui.label(RichText::new("Darkening").weak());
                ui.add(egui::Slider::new(&mut self.look.darkening, 0.0..=2.0))
                    .on_hover_text("How steeply it deepens past that");
            });

            ui.add_space(8.0);
            ui.separator();
            let drawn: usize = self.placed.iter().flatten().map(Vec::len).sum();
            let ready = self
                .models
                .iter()
                .filter(|model| matches!(model.state, State::Ready))
                .count();
            facts(
                ui,
                "scene_counts",
                &[
                    ("Placed", self.placements.len().to_string()),
                    ("Drawn", drawn.to_string()),
                    ("Waiting on a model", self.absent.to_string()),
                    ("Models", format!("{ready} of {}", self.models.len())),
                    ("Groups to read", self.waiting.len().to_string()),
                    (
                        "Materials",
                        format!("{} of {}", self.translated.len(), self.materials.len()),
                    ),
                    (
                        "Lights",
                        format!("{} of {}", self.lamps().len(), self.lights.len()),
                    ),
                    ("Wind", {
                        let count = self.waving.len();
                        let plural = match count {
                            1 => "",
                            _ => "s",
                        };
                        match self.ambient.wind() {
                            Some(held) => format!(
                                "clock {:.1}s, reach {:.2} at {:.0} deg, {:.2} rad/s, {count} material{plural}",
                                self.clock / TICKS,
                                held.reach,
                                held.heading.x.atan2(held.heading.z).to_degrees(),
                                held.rate,
                            ),
                            None => format!("no wind set stated, {count} material{plural}"),
                        }
                    }),
                    (
                        "Exposure",
                        match self.exposure.is_some() {
                            true => {
                                let held = self.renderer.lock().unwrap();
                                format!(
                                    "{:.3} from a frame measuring {:.3}",
                                    held.exposed(),
                                    held.measured()
                                )
                            }
                            false => "not run".to_owned(),
                        },
                    ),
                    // Which of the passes past the lighting ran. A weather that names no clouds
                    // draws none, and so does a draw that quietly went wrong; only the graph knows
                    // which of the two a frame without any is.
                    // How much of the sky reaches each part, which the zone's own `.svb` states by
                    // the same key an `.lcb` reaches a light by. A part it does not name stands in
                    // full sky, so a file that matches nothing looks exactly like no file at all.
                    // A zone with no grass of its own and a grass file that would not read look the
                    // same from the outside, and so does a grid nothing has asked for yet.
                    ("Grass", match &self.grass {
                        Grass::Wanted(_) => "waiting on the zone's own file".to_owned(),
                        Grass::Fetching(_, _) => "reading the zone's own file".to_owned(),
                        Grass::Done => "none".to_owned(),
                        Grass::Placing(held) => {
                            let read = held.grids.iter().filter(|grid| grid.taken).count();
                            let over = match self.unsown {
                                0 => String::new(),
                                held => format!(", {held} over the cap"),
                            };
                            format!(
                                "{read} of {} grids, {} models, {} placed, {} blades{over}",
                                held.grids.len(),
                                held.models.len(),
                                self.layers.get(held.layer).map_or(0, |held| held.placements),
                                self.blades,
                            )
                        }
                    }),
                    (
                        "Sky visibility",
                        format!(
                            "{} of {} placed",
                            self.placements
                                .iter()
                                .filter(|held| self.visibility.contains_key(&held.key))
                                .count(),
                            self.placements.len()
                        ),
                    ),
                    ("Blended materials", {
                        let mut tally: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
                        for (at, (_, slot)) in self.materials.iter().enumerate() {
                            let Slot::Ready(material) = slot else { continue };
                            let name = material.package();
                            if !wet_name(&name) {
                                continue;
                            }
                            let held = tally.entry(name).or_default();
                            held.0 += 1;
                            match self.translated.get(&at) {
                                Some(one) if one.resolve.is_some() => held.1 += 1,
                                Some(_) => held.2 += 1,
                                None => {}
                            }
                        }
                        match tally.is_empty() {
                            true => "none named".to_owned(),
                            false => tally
                                .iter()
                                .map(|(name, (all, wet, dry))| {
                                    format!("{name} {all}: {wet} blended, {dry} opaque")
                                })
                                .collect::<Vec<_>>()
                                .join(", "),
                        }
                    }),
                    (
                        "Blended surfaces",
                        format!(
                            "{} of {} translated",
                            self.translated.values().filter(|held| held.resolve.is_some()).count(),
                            self.translated.len()
                        ),
                    ),
                    (
                        "Shadow pass",
                        match (
                            self.packages.get(program::SHADOW),
                            self.lighting.as_ref().map(|held| held.shadow.is_some()),
                        ) {
                            (Some(Package::Ready(_)), Some(true)) => {
                                let reaches: Vec<String> = (0..program::SPLITS)
                                    .map(|at| format!("{:.0}", program::shadow_reach(at)))
                                    .collect();
                                format!(
                                    "translated, {} splits reaching {} (the game draws 5)",
                                    program::SPLITS,
                                    reaches.join(", ")
                                )
                            }
                            (Some(Package::Ready(_)), _) => "arrived, not translated".to_owned(),
                            (Some(Package::Failed), _) => "failed".to_owned(),
                            (Some(Package::Fetching(_)), _) => "fetching".to_owned(),
                            (Some(Package::Wanted), _) => "wanted".to_owned(),
                            (None, _) => "never asked for".to_owned(),
                        },
                    ),
                    ("Passes", {
                        let held = self.renderer.lock().unwrap().drawn();
                        let ran: Vec<&str> = [
                            (held.shadow, "shadow"),
                            (held.sky, "sky"),
                            (held.sun, "sun"),
                            (held.moon, "moon"),
                            (held.clouds[0], "band"),
                            (held.clouds[1], "sheet"),
                            (held.fog, "fog"),
                            (held.vignette, "vignette"),
                        ]
                        .into_iter()
                        .filter_map(|(ran, name)| ran.then_some(name))
                        .collect();
                        match ran.is_empty() {
                            true => "none".to_owned(),
                            false => ran.join(", "),
                        }
                    }),
                    (
                        "Textures",
                        format!(
                            "{}, {}, {} with slices",
                            self.textures.len(),
                            crate::assets::Bytes(self.resident),
                            self.stacked
                                .values()
                                .filter(|held| matches!(held, Stack::Ready))
                                .count(),
                        ),
                    ),
                ],
            );

            ui.add_space(8.0);
            ui.separator();
            changed |= self.ambient.ui(ui, follow, deps, backend);

            ui.add_space(8.0);
            ui.separator();
            section(ui, "Layers");
            ui.horizontal(|ui| {
                if ui.button("All").clicked() {
                    for layer in &mut self.layers {
                        layer.shown = true;
                    }
                    changed = true;
                }
                if ui.button("None").clicked() {
                    for layer in &mut self.layers {
                        layer.shown = false;
                    }
                    changed = true;
                }
            });
            ui.add_space(4.0);
            for layer in &mut self.layers {
                let mut label = format!("{} ({})", layer.name, layer.placements);
                if layer.festival != 0 {
                    label.push_str(&format!("  festival {}", layer.festival));
                }
                let mut hover = match layer.visible {
                    true => "drawn by default".to_owned(),
                    false => "hidden by default".to_owned(),
                };
                if let Some(origin) = &layer.origin {
                    hover.push('\n');
                    hover.push_str(origin);
                }
                changed |= ui
                    .checkbox(&mut layer.shown, RichText::new(label).monospace())
                    .on_hover_text(hover)
                    .changed();
            }
        });
        if refit {
            self.fit();
        }
        if changed {
            self.dirty = true;
        }
    }
}

pub fn ui(ui: &mut egui::Ui, scene: &mut Scene, backend: &Backend) {
    if let Some(why) = scene.renderer.lock().unwrap().failure() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(Color32::RED, format!("Could not build the shader: {why}"));
        });
        return;
    }
    scene.poll(ui, backend);
    scene.viewport(ui);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Euler order the files are read under. A pure yaw reduces to `Mat3::from_rotation_y`,
    /// which is what ring tests over the corpus settled; this pins the rest of it.
    #[test]
    fn a_rotation_turns_about_x_first() {
        let quarter = std::f32::consts::FRAC_PI_2;
        assert!((rotation([0.0, quarter, 0.0]) * Vec3::Z - Vec3::X).length() < 1e-5);
        assert!((rotation([quarter, 0.0, quarter]) * Vec3::Z - Vec3::X).length() < 1e-5);
    }

    #[test]
    fn a_fit_leaves_out_what_sits_nowhere_near_the_rest() {
        let mut points: Vec<Vec3> = (0..100).map(|at| Vec3::new(at as f32, 0.0, 0.0)).collect();
        points.push(Vec3::splat(1_400_000.0));
        let (center, reach) = bulk(&points);
        assert!((center - Vec3::new(50.0, 0.0, 0.0)).length() < 5.0);
        assert!(reach < 100.0);
    }

    #[test]
    fn a_model_falls_back_to_the_level_it_has() {
        assert_eq!(level([true, false, false], 0.0001), Some(0));
        assert_eq!(level([true, true, true], 0.5), Some(0));
        assert_eq!(level([true, true, true], 0.0001), Some(2));
        assert_eq!(level([false, false, false], 0.5), None);
    }
}
