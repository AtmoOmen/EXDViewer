//! What an effect does, read out of the tags it is written under and stepped a frame at a time.
//!
//! A scheduler starts a timeline, a timeline runs an emitter over a span of frames, and an emitter
//! bursts particles at an interval. A particle carries its own velocity forward and reads its
//! position, rotation, scale and color off curves indexed by how long it has been alive.
//!
//! What the tags mean comes from VFXEditor, which is the only place they are named. Only the ones
//! the corpus actually writes are read here; the rest of a particle's `Data` goes unread, so a kind
//! that draws a ribbon along its own path or warps what is behind it falls back to the sprite the
//! rest use. Nothing random is read: the `R`-suffixed curves beside the ones below go unread, so an
//! effect plays the same way every time and scrubbing back to a frame lands where it did before.

use glam::{Quat, Vec3, Vec4};
use ironworks::file::avfx::{Avfx, Block, DirectionalLightSource, Item, Model as Geometry};

use super::curve::{self, Curve};
use super::find;
use super::program::{self, UV_REGISTERS, UV_SETS};

/// Live particles and running emitters one effect may hold. Both counts come off the file unchecked
/// and this ships to a browser, where an effect asking for millions takes the tab with it.
const PARTICLES: usize = 8192;
const EMITTERS: usize = 512;

/// How deep an emitter may spawn another.
const DEPTH: u8 = 4;

/// Frames a loop runs for where nothing in the file bounds it, and the longest one it may reach.
const LOOP: i32 = 300;
const LONGEST: i32 = 3600;

/// Frames a fit is taken over.
const FITTED: i32 = 300;

/// The Euler order `apricot_powder.shpk` builds a particle's basis under: about Z first, then X,
/// then Y.
fn rotation(angles: Vec3) -> Quat {
    Quat::from_rotation_y(angles.y)
        * Quat::from_rotation_x(angles.x)
        * Quat::from_rotation_z(angles.z)
}

fn integer(blocks: &[Block], name: &str) -> Option<i32> {
    find(blocks, name)?.i32()
}

fn nested<'a>(blocks: &'a [Block], name: &str) -> &'a [Block] {
    find(blocks, name).map_or(&[][..], Block::blocks)
}

/// A tag naming one of the effect's lists. These are written as a list of one-byte indices where
/// the tag allows several, and as a plain integer where it does not.
fn index(blocks: &[Block], name: &str) -> Option<usize> {
    let block = find(blocks, name)?;
    let value = match block.bytes() {
        [only] => i32::from(*only),
        bytes if bytes.len() == 4 => block.i32()?,
        [first, ..] => i32::from(*first),
        [] => return None,
    };
    usize::try_from(value).ok()
}

/// Whether something the file can switch off is switched off.
fn off(blocks: &[Block], name: &str) -> bool {
    find(blocks, name).and_then(Block::bool) == Some(false)
}

/// How long something lives, in frames. A life it never reaches is written as `-1`.
fn life(blocks: &[Block]) -> Option<f32> {
    let value = find(blocks, "Life")?.find("Val")?.f32()?;
    (value >= 0.0).then_some(value)
}

/// One animated value, or the constant the file leaves where it writes no curve.
struct Track {
    curve: Option<Curve>,
    idle: f32,
}

impl Track {
    fn read(blocks: &[Block], name: &str, idle: f32) -> Self {
        Self {
            curve: find(blocks, name).and_then(curve::read),
            idle,
        }
    }

    fn at(&self, frame: f32) -> f32 {
        self.curve
            .as_ref()
            .map_or(self.idle, |curve| curve.sample(frame)[2])
    }
}

fn triple(blocks: &[Block], names: [&str; 3], idle: f32) -> [Track; 3] {
    names.map(|name| Track::read(blocks, name, idle))
}

fn read(tracks: &[Track; 3], frame: f32) -> Vec3 {
    Vec3::from(tracks.each_ref().map(|track| track.at(frame)))
}

/// Which curve each axis reads, `ACT`. An axis tied to another is written no curve of its own, so
/// leaving it at the idle value is what makes a sprite whose file animates only its width come out
/// a fixed height.
fn tied<const N: usize>(blocks: &[Block]) -> [usize; N] {
    let mut out = std::array::from_fn(|axis| axis);
    let (from, onto): (usize, &[usize]) = match (N, integer(blocks, "ACT").unwrap_or_default()) {
        (2, 1) => (0, &[1]),
        (2, 2) => (1, &[0]),
        (_, 1) => (0, &[1, 2]),
        (_, 2) => (0, &[1]),
        (_, 3) => (0, &[2]),
        (_, 4) => (1, &[0, 2]),
        (_, 5) => (1, &[0]),
        (_, 6) => (1, &[2]),
        (_, 7) => (2, &[0, 1]),
        (_, 8) => (2, &[0]),
        (_, 9) => (2, &[1]),
        _ => return out,
    };
    for &axis in onto.iter().filter(|&&axis| axis < N) {
        out[axis] = from;
    }
    out
}

/// A value the file writes one curve an axis for, under a container of its own.
struct Axes {
    tracks: [Track; 3],
    tied: [usize; 3],
}

impl Axes {
    fn read(blocks: &[Block], name: &str, idle: f32) -> Self {
        let inner = nested(blocks, name);
        Self {
            tracks: triple(inner, ["X", "Y", "Z"], idle),
            tied: tied(inner),
        }
    }

    fn at(&self, frame: f32) -> Vec3 {
        Vec3::from(self.tied.map(|axis| self.tracks[axis].at(frame)))
    }
}

/// The same over two axes, which is how a uv set writes its scale and its scroll.
struct Pair {
    tracks: [Track; 2],
    tied: [usize; 2],
}

impl Pair {
    fn read(blocks: &[Block], name: &str, idle: f32) -> Self {
        let inner = nested(blocks, name);
        Self {
            tracks: ["X", "Y"].map(|axis| Track::read(inner, axis, idle)),
            tied: tied(inner),
        }
    }

    fn at(&self, frame: f32) -> [f32; 2] {
        self.tied.map(|axis| self.tracks[axis].at(frame))
    }
}

/// One of the up to four uv sets a particle carries, `UvSt`. The sprite packages read a coordinate
/// the viewer has already transformed and the model packages read the transform itself, so both take
/// the same two rows: `uv' = dot(vec3(uv, 1), row.xyw)`.
struct UvSet {
    scale: Pair,
    scroll: Pair,
    turn: Track,
}

impl UvSet {
    fn read(block: &Block) -> Self {
        let blocks = block.blocks();
        Self {
            scale: Pair::read(blocks, "Scl", 1.0),
            scroll: Pair::read(blocks, "Scr", 0.0),
            turn: Track::read(blocks, "Rot", 0.0),
        }
    }

    fn at(&self, frame: f32) -> [[f32; 4]; UV_REGISTERS] {
        let [width, height] = self.scale.at(frame);
        let [across, down] = self.scroll.at(frame);
        let (sin, cos) = self.turn.at(frame).sin_cos();
        let (a, b) = (cos * width, -sin * height);
        let (c, d) = (sin * width, cos * height);
        // The turn and the scale are about the texture's own middle, so a set that only spins keeps
        // what it was showing.
        [
            [a, b, 0.0, 0.5 - (a + b) * 0.5 + across],
            [c, d, 0.0, 0.5 - (c + d) * 0.5 + down],
        ]
    }
}

/// Every set a particle carries, as the registers a draw hands over.
fn transform(sets: &[UvSet], frame: f32) -> [[f32; 4]; UV_SETS * UV_REGISTERS] {
    let mut out = program::UV_IDENTITY;
    for (set, held) in sets.iter().take(UV_SETS).enumerate() {
        let rows = held.at(frame);
        out[set * UV_REGISTERS..][..UV_REGISTERS].copy_from_slice(&rows);
    }
    out
}

/// A color: three channels in one curve, with an alpha, a brightness and a per-channel scale
/// written beside them.
struct Tint {
    rgb: Option<Curve>,
    alpha: Track,
    brightness: Track,
    scale: [Track; 4],
}

impl Tint {
    fn read(blocks: &[Block], name: &str) -> Self {
        let inner = nested(blocks, name);
        Self {
            rgb: find(inner, "RGB").and_then(curve::read),
            alpha: Track::read(inner, "A", 1.0),
            brightness: Track::read(inner, "Bri", 1.0),
            scale: ["SclR", "SclG", "SclB", "SclA"].map(|name| Track::read(inner, name, 1.0)),
        }
    }

    fn at(&self, frame: f32) -> Vec4 {
        let rgb = self
            .rgb
            .as_ref()
            .map_or([1.0; 3], |curve| curve.sample(frame));
        let brightness = self.brightness.at(frame);
        Vec4::new(
            rgb[0] * brightness * self.scale[0].at(frame),
            rgb[1] * brightness * self.scale[1].at(frame),
            rgb[2] * brightness * self.scale[2].at(frame),
            self.alpha.at(frame) * self.scale[3].at(frame),
        )
    }
}

/// Where something sits, so a spawned thing can be placed under whatever spawned it.
#[derive(Clone, Copy)]
struct Place {
    origin: Vec3,
    turn: Quat,
    scale: Vec3,
}

impl Place {
    const NONE: Self = Self {
        origin: Vec3::ZERO,
        turn: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    fn under(&self, inner: Place) -> Place {
        Place {
            origin: self.origin + self.turn * (inner.origin * self.scale),
            turn: self.turn * inner.turn,
            scale: self.scale * inner.scale,
        }
    }
}

/// How a particle's color reaches what is already drawn. `RMT`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Blend {
    Opaque,
    Alpha,
    Multiply,
    Screen,
    Subtract,
    Add,
}

impl From<i32> for Blend {
    fn from(value: i32) -> Self {
        match value {
            1 | 9 => Self::Multiply,
            2 | 10 => Self::Add,
            3 | 11 => Self::Subtract,
            4 | 12 => Self::Screen,
            8 => Self::Opaque,
            _ => Self::Alpha,
        }
    }
}

/// A world axis, as `RBDT` names one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// Which way a sprite is turned to be drawn, `RBDT`. The two that read a velocity are drawn against
/// the screen.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Facing {
    /// Set into the screen's own plane.
    Screen,
    /// Turned to look at the eye.
    Camera,
    /// Billed about the world's up axis, so it turns with the camera but never leans.
    Upright,
    /// Left lying in the world, across the two axes the one it names stands out of.
    Still(Axis),
}

impl Facing {
    /// What `RBDT` reads as for a particle of `kind`. A decal is cast onto what lies under it, so
    /// the axis it names settles nothing: it is scaled across x and z, where every other kind is
    /// scaled across the two axes the one it names leaves. Naming no base at all is not a default:
    /// the powder package turns a corner by the particle's own angles and never reads the view, so
    /// what names none is left in the plane its own rotation puts it in.
    fn read(kind: i32, base: i32) -> Self {
        match (kind, base) {
            (10..=12, 0..=2 | 10) => Self::Still(Axis::Y),
            (_, 0) => Self::Still(Axis::X),
            (_, 1) => Self::Still(Axis::Y),
            (_, 2 | 10) => Self::Still(Axis::Z),
            (_, 4 | 8 | 9) => Self::Upright,
            (_, 6) => Self::Camera,
            _ => Self::Screen,
        }
    }
}

/// What a particle draws as.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Shape {
    /// A quad turned to face the camera.
    Sprite,
    /// One of the effect's own models, indexed into [`Effect::models`].
    Model(usize),
}

/// One of the texture roles a particle names, as the package's own sampler is called.
const ROLES: [&str; 8] = [
    "g_SamplerColor1",
    "g_SamplerColor2",
    "g_SamplerColor3",
    "g_SamplerColor4",
    "g_SamplerNormal",
    "g_SamplerDistortion",
    "g_SamplerPalette",
    "g_SamplerReflection_",
];

/// The tags each of those roles is written under.
const SETS: [&str; 8] = ["TC1", "TC2", "TC3", "TC4", "TN", "TD", "TP", "TR"];

/// How a texture set combines with what came before it. `TCCT` and `TCAT`, whose orders VFXEditor
/// names and which the package's own key values follow one for one.
const CALCULATE_COLOR: [&str; 6] = ["Mul", "Add", "Sub", "Max", "Min", "None"];
const CALCULATE_ALPHA: [&str; 4] = ["Mul", "Max", "Min", "None"];

/// How a texture is filtered and wrapped. `TFT` runs from off through three degrees of anisotropy,
/// none of which GL ES has, so anything past off is filtered.
const WRAPS: [u32; 3] = [glow::REPEAT, glow::CLAMP_TO_EDGE, glow::MIRRORED_REPEAT];

/// What a particle asks its shader package for: the keys its own texture sets resolve to, and which
/// of the effect's textures fills each role the package names.
pub struct Shading {
    pub keys: Vec<(u32, u32)>,
    /// The light keys, kept apart because they come off the effect's own settings rather than the
    /// particle's, and a package that carries no such node should still draw the particle textured.
    pub lights: Vec<(u32, u32)>,
    /// The package's own sampler id, the effect's texture behind it, and how it is sampled.
    pub textures: Vec<(u32, usize, u32, [u32; 2])>,
    /// Whether this is drawn from a stream the viewer places in the world rather than from one of
    /// the effect's own models.
    pub sprite: bool,
}

struct Particle {
    life: Option<f32>,
    gravity: Track,
    drag: Track,
    position: Axes,
    rotation: Axes,
    scale: Axes,
    spin: [Track; 3],
    color: Tint,
    uv: Vec<UvSet>,
    texture: Option<usize>,
    shape: Shape,
    facing: Facing,
    blend: Blend,
    shading: std::sync::Arc<Shading>,
}

/// The keys and textures a particle's own texture sets and depth handling resolve to. Everything a
/// drawing package would read off an `.mtrl` an effect states here, and apricot declares no material
/// keys at all, so all of it lands in the scene group.
fn shading(block: &Block, lights: Option<Vec<(u32, u32)>>, sprite: bool) -> Shading {
    let blocks = block.blocks();
    let mut keys = Vec::new();
    let mut textures = Vec::new();
    let mut key = |name: &str, value: String| {
        keys.push((program::id(name), program::id(&value)));
    };

    let sets = integer(blocks, "UvSN").unwrap_or_default().clamp(0, 4);
    key("UvSetCount_Table", format!("UvSetCount_{sets}"));
    key(
        "DepthOffsetType_Table",
        match integer(blocks, "DOTy") == Some(1) {
            true => "DepthOffsetType_FixedIntervalNDC",
            false => "DepthOffsetType_Legacy",
        }
        .to_owned(),
    );

    for (at, (tag, role)) in SETS.iter().zip(ROLES).enumerate() {
        let inner = nested(blocks, tag);
        let held = index(inner, "TxNo").or_else(|| index(inner, "TLst"));
        let name = match at {
            0..=3 => format!("TextureColor{}", at + 1),
            4 => "TextureNormal".to_owned(),
            5 => "TextureDistortion".to_owned(),
            6 => "TexturePalette".to_owned(),
            _ => "TextureReflection".to_owned(),
        };
        let on = find(inner, "bEna").and_then(Block::bool) == Some(true) && held.is_some();
        key(
            &format!("{name}_Table"),
            format!("{name}_{}", if on { "Enable" } else { "Disable" }),
        );
        if !on {
            continue;
        }
        let uv = integer(inner, "UvSN").unwrap_or_default().clamp(0, 3);
        // The palette is a lookup rather than a surface, so it has no uv set of its own.
        if at != 6 {
            key(&format!("{name}_UvNo_Table"), format!("{name}_Uv_{uv}"));
        }
        if at <= 3 {
            key(
                &format!("{name}_ColorToAlpha_Table"),
                format!(
                    "{name}_ColorToAlpha_{}",
                    match find(inner, "bC2A").and_then(Block::bool) == Some(true) {
                        true => "On",
                        false => "Off",
                    }
                ),
            );
        }
        // The first color set is what the others are combined into, so it has no arithmetic.
        let combine = |table: &[&str], tag: &str| {
            let held = integer(inner, tag).unwrap_or_default();
            table
                .get(usize::try_from(held).unwrap_or(0))
                .copied()
                .unwrap_or(table[0])
                .to_owned()
        };
        if (1..=3).contains(&at) || at == 7 {
            key(
                &format!("{name}_CalculateColor_Table"),
                format!(
                    "{name}_CalculateColor_{}",
                    combine(&CALCULATE_COLOR, "TCCT")
                ),
            );
        }
        if (1..=3).contains(&at) {
            key(
                &format!("{name}_CalculateAlpha_Table"),
                format!(
                    "{name}_CalculateAlpha_{}",
                    combine(&CALCULATE_ALPHA, "TCAT")
                ),
            );
        }
        if at == 5 {
            for set in 0..UV_SETS {
                let on = find(inner, &format!("bT{}", set + 1)).and_then(Block::bool) == Some(true);
                key(
                    &format!("TextureDistortion_UvSet{set}_Table"),
                    format!(
                        "TextureDistortion_UvSet_{}",
                        if on { "Enable" } else { "Disable" }
                    ),
                );
            }
        }
        let wrap = |tag: &str| {
            let held = integer(inner, tag).unwrap_or_default();
            WRAPS
                .get(usize::try_from(held).unwrap_or(0))
                .copied()
                .unwrap_or(glow::REPEAT)
        };
        let filter = match integer(inner, "TFT").unwrap_or(1) > 0 {
            true => glow::LINEAR,
            false => glow::NEAREST,
        };
        textures.push((
            program::id(role),
            held.unwrap_or_default(),
            filter,
            [wrap("TBUT"), wrap("TBVT")],
        ));
    }
    Shading {
        keys,
        lights: lights.unwrap_or_default(),
        textures,
        sprite,
    }
}

impl Particle {
    fn read(block: &Block, models: usize, lights: &[(u32, u32)]) -> Self {
        let blocks = block.blocks();
        let data = nested(blocks, "Data");
        let model = |name| match index(data, name) {
            Some(model) if model < models => Shape::Model(model),
            _ => Shape::Sprite,
        };
        let kind = integer(blocks, "PrVT").unwrap_or_default();
        // The kinds that draw geometry name it under a tag of their own.
        let shape = match kind {
            5 | 14 => model("MdNo"),
            13 => model("MNO"),
            _ => Shape::Sprite,
        };
        let sprite = shape == Shape::Sprite;
        Self {
            life: life(blocks),
            shading: std::sync::Arc::new(shading(
                block,
                (!sprite).then(|| lights.to_vec()),
                sprite,
            )),
            shape,
            gravity: Track::read(blocks, "Gra", 0.0),
            drag: Track::read(blocks, "ARs", 0.0),
            position: Axes::read(blocks, "Pos", 0.0),
            rotation: Axes::read(blocks, "Rot", 0.0),
            scale: Axes::read(blocks, "Scl", 1.0),
            spin: triple(blocks, ["VRX", "VRY", "VRZ"], 0.0),
            color: Tint::read(blocks, "Col"),
            uv: blocks
                .iter()
                .filter(|block| block.name() == "UvSt")
                .take(UV_SETS)
                .map(UvSet::read)
                .collect(),
            texture: index(nested(blocks, "TC1"), "TLst"),
            facing: Facing::read(kind, integer(blocks, "RBDT").unwrap_or_default()),
            blend: integer(blocks, "RMT").unwrap_or_default().into(),
        }
    }
}

/// The light keys an effect's own settings resolve to. Where a file names no light the package
/// defaults answer, and those draw an effect unlit.
fn lights(file: &Avfx) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut key = |name: &str, value: &str| out.push((program::id(name), program::id(value)));
    if !matches!(
        file.directional_light_source(),
        None | Some(DirectionalLightSource::None)
    ) {
        key("DirectionalLight_Table", "DirectionalLight_Enable");
    }
    let held = file
        .point_light_sources()
        .iter()
        .filter(|source| {
            !matches!(
                source,
                None | Some(ironworks::file::avfx::PointLightSource::None)
            )
        })
        .count();
    if held > 0 {
        key(
            "PointLightCount_Table",
            match held {
                1 => "PointLightCount_1_0",
                _ => "PointLightCount_1_1",
            },
        );
    }
    out
}

/// One entry of an emitter's particle or emitter list.
struct Spawn {
    target: usize,
    count: i32,
    delay: f32,
}

impl Spawn {
    fn read(item: &Item, of: usize) -> Option<Self> {
        let blocks = item.blocks();
        let target = usize::try_from(integer(blocks, "TgtB")?).ok()?;
        (target < of && !off(blocks, "bEnb")).then(|| Self {
            target,
            count: integer(blocks, "CrCn").unwrap_or(1).clamp(0, 64),
            delay: integer(blocks, "GenD").unwrap_or_default() as f32,
        })
    }
}

struct Emitter {
    life: Option<f32>,
    count: Track,
    interval: Track,
    position: Axes,
    rotation: Axes,
    scale: Axes,
    color: Tint,
    /// `Data/IjS`, how fast a particle leaves, along the direction `Data/AnX`..`AnZ` turns `+Y` to.
    speed: Track,
    heading: [Track; 3],
    particles: Vec<Spawn>,
    emitters: Vec<Spawn>,
}

impl Emitter {
    fn read(emitter: &ironworks::file::avfx::Emitter, particles: usize, emitters: usize) -> Self {
        let blocks = emitter.properties();
        let data = nested(blocks, "Data");
        Self {
            life: life(blocks),
            count: Track::read(blocks, "CrC", 1.0),
            interval: Track::read(blocks, "CrI", 1.0),
            position: Axes::read(blocks, "Pos", 0.0),
            rotation: Axes::read(blocks, "Rot", 0.0),
            scale: Axes::read(blocks, "Scl", 1.0),
            color: Tint::read(blocks, "Col"),
            speed: Track::read(data, "IjS", 0.0),
            heading: triple(data, ["AnX", "AnY", "AnZ"], 0.0),
            particles: emitter
                .particles()
                .iter()
                .filter_map(|item| Spawn::read(item, particles))
                .collect(),
            emitters: emitter
                .emitters()
                .iter()
                .filter_map(|item| Spawn::read(item, emitters))
                .collect(),
        }
    }
}

/// One emitter a timeline runs, and the frames it runs over.
struct Run {
    emitter: usize,
    start: i32,
    until: i32,
}

/// The emitters one timeline runs, added to `runs` at `at`.
fn timeline(file: &Avfx, index: usize, at: i32, runs: &mut Vec<Run>) {
    let Some(timeline) = file.timelines().get(index) else {
        return;
    };
    for item in timeline.items() {
        let blocks = item.blocks();
        if off(blocks, "bEna") {
            continue;
        }
        let Some(emitter) = integer(blocks, "EmNo")
            .and_then(|value| usize::try_from(value).ok())
            .filter(|&emitter| emitter < file.emitters().len())
        else {
            continue;
        };
        let end = integer(blocks, "EdTm").unwrap_or(-1);
        runs.push(Run {
            emitter,
            start: at + integer(blocks, "StTm").unwrap_or_default(),
            until: match end < 0 {
                true => i32::MAX,
                false => at + end,
            },
        });
    }
}

fn runs(file: &Avfx) -> Vec<Run> {
    let mut runs = Vec::new();
    for scheduler in file.schedulers() {
        for item in scheduler.items() {
            let blocks = item.blocks();
            if off(blocks, "bEna") {
                continue;
            }
            let Some(index) = integer(blocks, "TlNo").and_then(|value| usize::try_from(value).ok())
            else {
                continue;
            };
            timeline(
                file,
                index,
                integer(blocks, "StTm").unwrap_or_default(),
                &mut runs,
            );
        }
    }
    // An effect whose schedulers start nothing still holds the timelines and emitters it would
    // have run, and is worth showing rather than leaving blank.
    if runs.is_empty() {
        for index in 0..file.timelines().len() {
            timeline(file, index, 0, &mut runs);
        }
    }
    if runs.is_empty() {
        runs.extend((0..file.emitters().len()).map(|emitter| Run {
            emitter,
            start: 0,
            until: i32::MAX,
        }));
    }
    runs
}

/// A model vertex as the game's own shaders read it: four uv sets, and a normal and tangent the
/// shader takes the bias off itself, which is why they go up as the bytes the file holds rather than
/// as the signed values ironworks reads them into.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 4],
    pub normal: [u8; 4],
    pub tangent: [u8; 4],
    pub color: [u8; 4],
    pub uv: [f32; 8],
}

pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
}

fn mesh(model: &Geometry) -> Mesh {
    let biased = |held: [i8; 4]| held.map(|lane| (lane as u8).wrapping_add(128));
    Mesh {
        vertices: model
            .vertices()
            .iter()
            .map(|vertex| Vertex {
                position: vertex.position(),
                normal: biased(vertex.normal()),
                tangent: biased(vertex.tangent()),
                color: vertex.colour(),
                uv: std::array::from_fn(|lane| vertex.uv()[lane / 2][lane % 2]),
            })
            .collect(),
        indices: model
            .triangles()
            .iter()
            .flat_map(|triangle| triangle.indices())
            .collect(),
    }
}

/// One emitter running: a timeline started it, or a parent emitter did.
struct Running {
    def: usize,
    born: i32,
    until: i32,
    place: Place,
    tint: Vec4,
    /// Frames since the last burst.
    since: f32,
    depth: u8,
}

struct Live {
    def: usize,
    born: i32,
    life: f32,
    /// How far it has carried itself under its own velocity, in the frame it was spawned into.
    at: Vec3,
    velocity: Vec3,
    /// Where the emitter stood when it spawned, which its own curves run under.
    place: Place,
    tint: Vec4,
}

pub struct State {
    pub frame: i32,
    running: Vec<Running>,
    particles: Vec<Live>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            // Nothing has run yet, so the first step lands on frame zero.
            frame: -1,
            running: Vec::new(),
            particles: Vec::new(),
        }
    }
}

/// One thing to draw, in the effect's own space.
#[derive(Clone, Copy)]
pub struct Drawn {
    pub center: [f32; 3],
    pub scale: [f32; 3],
    pub turn: [f32; 4],
    /// How far the sprite is spun in the plane it is billed onto, which is the one part of its turn
    /// a quad facing the camera can carry.
    pub roll: f32,
    pub color: [f32; 4],
    /// What each uv set does to a texture coordinate, two registers a set.
    pub uv: [[f32; 4]; UV_SETS * UV_REGISTERS],
    pub texture: Option<usize>,
    pub shape: Shape,
    pub facing: Facing,
    pub blend: Blend,
    /// Which of the effect's particles this is one of, which is what its shading is read off.
    pub def: usize,
}

impl Drawn {
    /// Carried into a placement external to the effect itself: a zone stands its own copy wherever
    /// an instance says, so what the emitters ran out in their own space is turned by the
    /// placement's rotation and scale before it is offset into the world, and tinted by whatever
    /// colour and distance fade the placement itself states.
    pub(crate) fn placed(mut self, rotation: Quat, offset: Vec3, scale: f32, tint: Vec4) -> Self {
        self.center = (offset + rotation * (Vec3::from(self.center) * scale)).to_array();
        self.turn = (rotation * Quat::from_array(self.turn)).to_array();
        self.scale = (Vec3::from(self.scale) * scale).to_array();
        self.color = (Vec4::from(self.color) * tint).to_array();
        self
    }
}

pub struct Effect {
    emitters: Vec<Emitter>,
    particles: Vec<Particle>,
    runs: Vec<Run>,
    /// The `.atex` files the particles sample, in the order they index them.
    pub textures: Vec<String>,
    pub models: Vec<Mesh>,
    /// Frames the effect runs for before it starts over. Only meaningful where `bounded` is true;
    /// where it is not, this is `LOOP`, a fallback for scrubbing the file on its own rather than a
    /// span the effect actually starts over at.
    pub length: i32,
    /// Whether every run the file states truly ends and every particle it can spawn has a life of
    /// its own, so nothing outlives the point `length` names. An effect a placement runs forever
    /// has no such point: it settles once its emitters stop spawning and holds there, and wrapping
    /// its frame back to zero anyway restarts it from empty on a cycle nothing in the file states.
    pub bounded: bool,
}

impl Effect {
    pub fn read(file: &Avfx) -> Self {
        let lights = lights(file);
        let particles: Vec<Particle> = file
            .particles()
            .iter()
            .map(|particle| Particle::read(particle, file.models().len(), &lights))
            .collect();
        let emitters: Vec<Emitter> = file
            .emitters()
            .iter()
            .map(|emitter| Emitter::read(emitter, particles.len(), file.emitters().len()))
            .collect();
        let runs = runs(file);

        // A timeline item's own end is where the effect it placed is done, not a lower bound a
        // particle's own life can run past: an `EdTm` an artist tunes to the effect's length would
        // otherwise need every particle's life hand-matched to it as well. A particle with no life
        // of its own still runs to whatever that end comes out to, via `length` below.
        let bounded = runs.iter().all(|run| run.until != i32::MAX)
            && particles.iter().all(|particle| particle.life.is_some());
        let length = match bounded {
            true => runs.iter().map(|run| run.until).max().unwrap_or_default(),
            false => LOOP,
        }
        .clamp(1, LONGEST);

        Self {
            emitters,
            particles,
            runs,
            textures: file.textures().to_vec(),
            models: file.models().iter().map(mesh).collect(),
            length,
            bounded,
        }
    }

    /// Steps to `frame`, replaying from the start where the state sits past it: a particle's
    /// position is the sum of every step it has taken, so there is no stepping backwards.
    pub fn seek(&self, state: &mut State, frame: i32) {
        if frame < state.frame {
            *state = State::default();
        }
        while state.frame < frame {
            self.step(state);
        }
    }

    fn step(&self, state: &mut State) {
        let frame = state.frame + 1;
        state.frame = frame;

        state.particles.retain_mut(|live| {
            let age = (frame - live.born) as f32;
            if age > live.life {
                return false;
            }
            let def = &self.particles[live.def];
            live.velocity *= (1.0 - def.drag.at(age)).clamp(0.0, 1.0);
            live.velocity.y -= def.gravity.at(age);
            live.at += live.velocity;
            true
        });

        for run in &self.runs {
            if run.start == frame && state.running.len() < EMITTERS {
                state.running.push(Running {
                    def: run.emitter,
                    born: frame,
                    until: run.until,
                    place: Place::NONE,
                    tint: Vec4::ONE,
                    since: f32::INFINITY,
                    depth: 0,
                });
            }
        }
        state.running.retain(|running| frame <= running.until);

        let mut spawned = Vec::new();
        let room = EMITTERS.saturating_sub(state.running.len());
        for running in &mut state.running {
            let def = &self.emitters[running.def];
            let local = (frame - running.born) as f32;
            if def.life.is_some_and(|life| local > life) {
                continue;
            }
            running.since += 1.0;
            if running.since < def.interval.at(local).max(1.0) {
                continue;
            }
            running.since = 0.0;

            let burst = def.count.at(local).round().clamp(0.0, 64.0) as i32;
            if burst == 0 {
                continue;
            }
            let place = running.place.under(Place {
                origin: def.position.at(local),
                turn: rotation(def.rotation.at(local)),
                scale: def.scale.at(local),
            });
            let tint = running.tint * def.color.at(local);
            let velocity = rotation(read(&def.heading, local)) * Vec3::Y * def.speed.at(local);

            for spawn in &def.particles {
                if local < spawn.delay {
                    continue;
                }
                // Infinite only where the emitter itself is bound to stop spawning; otherwise
                // nothing would ever cap how many pile up. Reaching here at all already means the
                // effect is unbounded, since a bounded one states a life on every particle.
                let life = self.particles[spawn.target]
                    .life
                    .unwrap_or(match def.life.is_some() {
                        true => f32::INFINITY,
                        false => self.length as f32,
                    });
                for _ in 0..burst * spawn.count {
                    if state.particles.len() >= PARTICLES {
                        break;
                    }
                    state.particles.push(Live {
                        def: spawn.target,
                        born: frame,
                        life,
                        at: Vec3::ZERO,
                        velocity,
                        place,
                        tint,
                    });
                }
            }

            if running.depth < DEPTH {
                for spawn in &def.emitters {
                    if local < spawn.delay || spawned.len() >= room {
                        break;
                    }
                    spawned.push(Running {
                        def: spawn.target,
                        born: frame,
                        until: self.emitters[spawn.target]
                            .life
                            .map_or(i32::MAX, |life| frame + life as i32),
                        place,
                        tint,
                        since: f32::INFINITY,
                        depth: running.depth + 1,
                    });
                }
            }
        }
        state.running.extend(spawned);
    }

    pub fn drawn(&self, state: &State) -> Vec<Drawn> {
        state
            .particles
            .iter()
            .map(|live| {
                let def = &self.particles[live.def];
                let age = (state.frame - live.born) as f32;
                let angles = def.rotation.at(age) + read(&def.spin, age) * age;
                let place = live.place.under(Place {
                    origin: live.at + def.position.at(age),
                    turn: rotation(angles),
                    scale: def.scale.at(age),
                });
                Drawn {
                    center: place.origin.to_array(),
                    scale: place.scale.to_array(),
                    turn: place.turn.to_array(),
                    roll: angles.z,
                    color: (live.tint * def.color.at(age)).to_array(),
                    uv: transform(&def.uv, age),
                    texture: def.texture,
                    shape: def.shape,
                    facing: def.facing,
                    blend: def.blend,
                    def: live.def,
                }
            })
            .collect()
    }

    /// What the shader package a particle is drawn with is asked for.
    pub fn shading(&self, def: usize) -> Option<std::sync::Arc<Shading>> {
        self.particles.get(def).map(|held| held.shading.clone())
    }

    /// A sphere the whole run fits inside, for the camera to open on. A scale is not an extent: a
    /// sprite is drawn one scale wide about its own center and only across the two axes it is billed
    /// onto, and a model is drawn its own geometry wide, so taking the scale for either stands the
    /// camera off by several times too far.
    pub fn fit(&self) -> (Vec3, f32) {
        let models: Vec<f32> = self
            .models
            .iter()
            .map(|mesh| {
                mesh.vertices
                    .iter()
                    .map(|vertex| Vec3::from_slice(&vertex.position).length())
                    .fold(0.0f32, f32::max)
            })
            .collect();

        let mut state = State::default();
        let (mut low, mut high) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for _ in 0..self.length.min(FITTED) {
            self.step(&mut state);
            for live in &state.particles {
                let def = &self.particles[live.def];
                let age = (state.frame - live.born) as f32;
                let at = live.place.origin
                    + live.place.turn * ((live.at + def.position.at(age)) * live.place.scale);
                let scale = (def.scale.at(age) * live.place.scale).abs();
                let reach = match def.shape {
                    Shape::Sprite => {
                        0.5 * match def.facing {
                            Facing::Still(Axis::X) => scale.y.max(scale.z),
                            Facing::Still(Axis::Y) => scale.x.max(scale.z),
                            _ => scale.x.max(scale.y),
                        }
                    }
                    Shape::Model(index) => {
                        scale.max_element() * models.get(index).copied().unwrap_or(0.5)
                    }
                }
                .max(0.05);
                low = low.min(at - reach);
                high = high.max(at + reach);
            }
        }
        match low.cmple(high).all() {
            true => (
                (low + high) * 0.5,
                ((high - low) * 0.5).max_element().max(0.1),
            ),
            false => (Vec3::ZERO, 1.0),
        }
    }
}
