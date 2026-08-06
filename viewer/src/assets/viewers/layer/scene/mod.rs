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

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use egui::{Color32, RichText, ScrollArea, Sense, TextureHandle, TextureOptions};
use glam::{Mat3, Mat4, Quat, Vec3};
use ironworks::file::layer::{InstanceData, LayerGroup, Transform};
use ironworks::file::mdl::{MeshKind, ModelContainer};
use ironworks::file::{File, layer, lcb, lgb::LayerGroupFile, sgb::SharedGroupFile, svb, tera};

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

/// Lights the frame draws at once. Every one is a pass of its own over the volume it reaches, so a
/// zone's whole set would cost more than it shows; the nearest are kept.
const LAMPS: usize = 48;

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

/// The keys the engine sets rather than the material. A package that declares none of them resolves
/// exactly as it did, since a key the package never declares is never looked up.
const KEYS: [(u32, u32); 2] = [
    (GET_NORMAL_MAP, GET_NORMAL_MAP_ON),
    (APPLY_ALPHA_CLIP, APPLY_ALPHA_CLIP_ON),
];

/// What a light is worth where the zone states no box for it. Nothing in the placement carries the
/// reach: the file's own `range` is one in nearly every light a zone places.
const REACH: f32 = 6.0;

/// Requests of each kind in flight at once.
const FILES: usize = 12;
const PACKAGES: usize = 4;
const MODELS: usize = 6;
const MATERIALS: usize = 16;
const TEXTURES: usize = 16;

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

/// One `BgPart`, in world space.
#[derive(Clone, Copy)]
struct Placement {
    model: usize,
    transform: Mat4,
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
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Decoding(Vec<u8>),
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
}

enum Slot {
    Wanted,
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Box<Material>),
    Failed,
}

/// A shader package, which many materials name the same one of.
enum Package {
    Wanted,
    Fetching(TrackedPromise<Result<Vec<u8>>>),
    Ready(Vec<u8>),
    Failed,
}

/// One material's shaders, and how much of the G-buffer they were translated for.
struct Translated {
    attachments: usize,
    buffer: Vec<Arc<program::Program>>,
    depth: Option<Arc<program::Program>>,
}

/// One light the zone places. The box it is clipped against is stated in its own space, so the
/// placement carries where it stands and the box how far it carries.
struct Light {
    placement: Mat4,
    center: Vec3,
    min: Vec3,
    max: Vec3,
    color: Vec3,
    /// How the zone's own `.lcb` reaches this light: the instance at the top of the tree, then an
    /// index per shared group under it.
    key: (u32, [u8; 4]),
}

/// A file the scene names beside itself and reads once: the boxes its lights are clipped against,
/// and how much of the sky reaches each of its parts.
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
    material_at: HashMap<String, usize>,
    packages: HashMap<String, Package>,
    translated: HashMap<usize, Translated>,
    tables: HashMap<usize, Arc<(Vec<u16>, usize, usize)>>,
    lighting: Option<Arc<mdl::gpu::Lighting>>,
    ambient: ambient::Ambient,
    lights: Vec<Light>,
    /// The box each light is clipped against, by the key its `.lcb` entry uses.
    clips: HashMap<(u32, [u8; 4]), (Vec3, Vec3)>,
    clip: Aside,
    /// How much of the sky reaches each part, by the key its `.svb` entry uses.
    visibility: HashMap<(u32, [u8; 4]), f32>,
    sky: Aside,
    falloff: Aside,
    textures: BTreeMap<String, Texture>,
    resident: usize,
    files: HashMap<String, Held>,
    waiting: Vec<Expand>,
    terrain: Terrain,
    /// Placements the view was last framed over, so a scene that arrived empty frames itself once
    /// its first file lands rather than leaving the camera at the origin.
    fitted: usize,
    renderer: Arc<Mutex<gpu::Renderer>>,
    /// Where each model stands at each detail level, as the last rebuild left them.
    placed: Vec<[Vec<program::Instance>; 3]>,
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
    let wanted = match apparent {
        size if size > DETAIL[0] => 0,
        size if size > DETAIL[1] => 1,
        _ => 2,
    };
    (wanted..3)
        .chain((0..wanted).rev())
        .find(|level| drawn[*level])
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
            material_at: HashMap::new(),
            packages: HashMap::new(),
            translated: HashMap::new(),
            tables: HashMap::new(),
            lighting: None,
            ambient: ambient::Ambient::new(source.scene()),
            lights: Vec::new(),
            clips: HashMap::new(),
            clip: aside(source.scene().map(layer::Scene::light_culling_path)),
            visibility: HashMap::new(),
            sky: aside(source.scene().map(layer::Scene::sky_visibility_path)),
            falloff: Aside::Wanted(mdl::deferred::RAMP.1.to_owned()),
            textures: BTreeMap::new(),
            resident: 0,
            files: HashMap::new(),
            waiting: Vec::new(),
            terrain: match root {
                Some(root) => Terrain::Wanted(format!("{root}/bgplate/terrain.tera")),
                None => Terrain::Done,
            },
            fitted: 0,
            renderer: gpu::Renderer::new(),
            placed: Vec::new(),
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
                    });
                }
            }
            _ => scene.walk(
                source.groups(),
                Mat4::IDENTITY,
                (0, [0; 4]),
                1.0,
                None,
                0,
                None,
            ),
        }
        scene.fit();
        scene
    }

    /// Reads placements out of a file's layers, queueing every shared group it names.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        groups: &[LayerGroup],
        transform: Mat4,
        key: (u32, [u8; 4]),
        scale: f32,
        under: Option<usize>,
        depth: u8,
        origin: Option<&str>,
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
                    let here = transform * matrix(placed);
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
                            self.lights.push(Light {
                                placement: here,
                                center: here.transform_point3(Vec3::ZERO),
                                min: Vec3::splat(-REACH),
                                max: Vec3::splat(REACH),
                                color: color * held.intensity(),
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

    /// Where every model stands for where the eye now is. The transforms go to the card each frame
    /// rather than here, since a record carries the object into view space and the camera turns.
    fn rebuild(&mut self) {
        let eye = self.camera.position;
        let mut placed: Vec<[Vec<program::Instance>; 3]> = (0..self.models.len())
            .map(|_| std::array::from_fn(|_| Vec::new()))
            .collect();
        for model in &mut self.models {
            model.nearest = f32::INFINITY;
        }
        self.absent = 0;

        for at in 0..self.placements.len() {
            let placement = self.placements[at];
            if !self.layers[placement.layer].shown {
                continue;
            }
            let span = (placement.center - eye).length() - placement.radius;
            if span > self.load || (placement.fade > 0.0 && span > placement.fade) {
                continue;
            }
            let model = &mut self.models[placement.model];
            model.nearest = model.nearest.min(span);
            if !matches!(model.state, State::Ready) {
                self.absent += 1;
                continue;
            }
            let Some(level) = level(model.drawn, placement.radius / span.max(0.01)) else {
                continue;
            };
            placed[placement.model][level].push(program::Instance {
                transform: placement.transform,
                sky_visibility: self.visibility.get(&placement.key).copied().unwrap_or(1.0),
            });
        }
        self.placed = placed;
        self.written = eye;
        self.dirty = false;
    }

    /// The lights the frame draws, nearest first. Each is clipped against the box its zone states
    /// for it, in the light's own space.
    fn lamps(&self) -> Vec<program::Lamp> {
        let eye = self.camera.position;
        let mut near: Vec<(f32, &Light)> = self
            .lights
            .iter()
            .map(|light| ((light.center - eye).length(), light))
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
                    color: light.color,
                }
            })
            .collect()
    }

    /// The files read once beside the scene, as they arrive: the boxes its lights are clipped
    /// against, how much of the sky reaches each of its parts, and the ramp their falloff comes off.
    fn load_asides(&mut self, backend: &Backend) {
        for held in [&mut self.clip, &mut self.sky, &mut self.falloff] {
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
        let falloff = taken(&mut self.falloff);

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
        if let Some((path, bytes)) = falloff {
            match mdl::layered(&bytes, &path) {
                Ok(held) => self
                    .renderer
                    .lock()
                    .unwrap()
                    .queue_supplied(mdl::deferred::RAMP.0, held),
                Err(why) => log::error!("assets/layer: {path}: {why}"),
            }
        }
    }

    /// Asks for whatever the scene still needs and takes in whatever arrived. Runs every frame.
    fn poll(&mut self, ui: &egui::Ui, backend: &Backend) {
        self.load_terrain(backend);
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
                .any(|model| matches!(model.state, State::Decoding(_)))
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
                ));
                false
            }
            // A file that would not arrive takes its subtree with it rather than being asked for
            // again every frame.
            Some(Held::Failed) => false,
            _ => true,
        });
        self.waiting = waiting;
        for (source, transform, key, scale, layer, depth) in ready {
            self.walk(source.groups(), transform, key, scale, layer, depth, None);
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
                    matches!(self.models[*at].state, State::Wanted)
                        && self.models[*at].nearest <= self.load
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
                self.models[at].state = State::Fetching(TrackedPromise::spawn_local(async move {
                    files.read(&path).await
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
                Ok(bytes) => State::Decoding(bytes.clone()),
                Err(why) => {
                    log::error!("assets/layer: {}: {why}", model.path);
                    State::Failed
                }
            };
        }

        let decoding: Vec<usize> = (0..self.models.len())
            .filter(|at| matches!(self.models[*at].state, State::Decoding(_)))
            .take(DECODES)
            .collect();
        for at in decoding {
            let State::Decoding(bytes) =
                std::mem::replace(&mut self.models[at].state, State::Failed)
            else {
                continue;
            };
            match self.decode(at, bytes) {
                Ok(()) => {
                    self.models[at].state = State::Ready;
                    self.dirty = true;
                }
                Err(why) => log::error!("assets/layer: {}: {why}", self.models[at].path),
            }
        }
    }

    /// Reads every detail level of a model and hands its geometry to the card.
    fn decode(&mut self, at: usize, bytes: Vec<u8>) -> Result<()> {
        let container = ModelContainer::read(Cursor::new(bytes))?;
        let mut levels = Vec::new();
        let mut meshes = Vec::new();
        let mut drawn = [false; 3];
        for level in 0..3u8 {
            let model = container.model(mdl::detail(level));
            let mut built = Vec::new();
            let mut used = Vec::new();
            for mesh in model.meshes() {
                if !mesh.kinds().contains(&MeshKind::Standard) {
                    continue;
                }
                let (Ok(attributes), Ok(indices)) = (mesh.attributes(), mesh.indices()) else {
                    continue;
                };
                let Ok(geometry) = mdl::build(&attributes, indices) else {
                    continue;
                };
                let name = mesh.material().unwrap_or_default();
                let resolved = mdl::material::path(&name).unwrap_or(name);
                used.push(self.material(&resolved));
                built.push(geometry);
            }
            drawn[usize::from(level)] = !built.is_empty();
            levels.push(built);
            meshes.push(used);
        }
        // A model may carry no standard mesh at any level, which plenty of terrain plates do not.
        // That is what the model holds rather than a failure to read it, and `drawn` already says so.
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

    /// The packages the ready materials name, plus the four the frame is lit and resolved with.
    fn load_packages(&mut self, backend: &Backend) {
        let mut wanted: Vec<String> = [
            program::VIEW_POSITION,
            program::DIRECTIONAL,
            program::POINT,
            program::COMPOSITE,
        ]
        .map(str::to_owned)
        .to_vec();
        wanted.extend(self.materials.iter().filter_map(|(_, slot)| match slot {
            Slot::Ready(material) => Some(material.package()),
            _ => None,
        }));
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
            *held = Package::Fetching(TrackedPromise::spawn_local(async move {
                files.read(&wanted).await
            }));
            fetching += 1;
        }

        for (path, held) in &mut self.packages {
            let Package::Fetching(promise) = held else {
                continue;
            };
            let Some(result) = promise.try_get() else {
                continue;
            };
            *held = match result {
                Ok(bytes) => Package::Ready(bytes.clone()),
                Err(why) => {
                    log::error!("assets/layer: {path}: {why}");
                    Package::Failed
                }
            };
        }
    }

    /// Every ready material's shaders, translated once its package has arrived. A context that
    /// turned out to write fewer of the G-buffer's targets at once has them translated again.
    fn translate(&mut self) {
        let attachments = self.renderer.lock().unwrap().attachments();
        if self.lighting.is_none() {
            let held = |path: &str, pass| {
                let Some(Package::Ready(bytes)) = self.packages.get(path) else {
                    return None;
                };
                program::Program::screen(bytes, pass, attachments)
                    .inspect_err(|why| log::warn!("assets/layer: {path}: {why}"))
                    .ok()
                    .map(Arc::new)
            };
            if let (Some(position), Some(directional), Some(point), Some(composite)) = (
                held(program::VIEW_POSITION, program::Pass::Lighting),
                held(program::DIRECTIONAL, program::Pass::Lighting),
                held(program::POINT, program::Pass::Lamp),
                held(program::COMPOSITE, program::Pass::Composite),
            ) {
                self.lighting = Some(Arc::new(mdl::gpu::Lighting {
                    position,
                    directional,
                    point,
                    composite,
                }));
            }
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
            let page = |page| {
                program::Program::build(
                    bytes,
                    material,
                    &KEYS,
                    program::Pass::Buffer,
                    program::SUB_VIEW_MAIN,
                    page,
                    attachments,
                )
            };
            let Ok(first) = page(0).inspect_err(|why| {
                log::warn!("assets/layer: {}: {why}", material.package());
            }) else {
                continue;
            };
            let pages = first.outputs.len().div_ceil(attachments.max(1)).max(1);
            let mut buffer = vec![Arc::new(first)];
            buffer.extend((1..pages).filter_map(|held| page(held).ok().map(Arc::new)));
            let depth = program::Program::build(
                bytes,
                material,
                &KEYS,
                program::Pass::Depth,
                program::SUB_VIEW_MAIN,
                0,
                attachments,
            );
            self.translated.insert(
                at,
                Translated {
                    attachments,
                    buffer,
                    depth: depth.ok().map(Arc::new),
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

    /// Every texture the ready materials name, since the game's own shaders read all of them. Held
    /// for the whole scene rather than per model, since a zone's models share theirs heavily.
    fn load_textures(&mut self, ui: &egui::Ui, backend: &Backend) {
        let mut fetching = self
            .textures
            .values()
            .filter(|texture| matches!(texture, Texture::Fetching(_)))
            .count();
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
            .filter(|path| !self.textures.contains_key(*path))
            .cloned()
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
            self.textures.insert(
                path,
                Texture::Fetching(TrackedPromise::spawn_local(async move {
                    files.read_texture(&held, Some(TEXTURE_SIZE)).await
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
                ..Default::default()
            },
            lighting: self.lighting.clone(),
            lamps: self.lamps(),
            batches,
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

    fn surface(&self, slot: usize) -> gpu::Surface {
        let held = self.materials.get(slot).and_then(|(_, held)| match held {
            Slot::Ready(material) => Some(material),
            _ => None,
        });
        let bind = |path: &str| match self.textures.get(path) {
            Some(Texture::Ready(handle)) => Some(handle.id()),
            _ => None,
        };
        // Bare geometry until the material and its package arrive, rather than a hole where they
        // will be.
        let shaded = held
            .zip(self.translated.get(&slot))
            .map(|(material, held)| mdl::gpu::Shaded {
                buffer: held.buffer.clone(),
                depth: held.depth.clone(),
                resolve: None,
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

    pub fn details_ui(
        &mut self,
        ui: &mut egui::Ui,
        follow: &mut Option<String>,
        deps: &mut Deps,
        backend: &Backend,
    ) {
        let mut refit = false;
        let mut changed = false;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            section(ui, "View");
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
                    (
                        "Textures",
                        format!(
                            "{}, {}",
                            self.textures.len(),
                            crate::assets::Bytes(self.resident)
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
