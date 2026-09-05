//! The tree `.lgb` and `.sgb` hold, and the scene `.sgb` and `.lvb` wrap it in: a group of layers,
//! each holding placed instances, beside the files the zone is drawn from.
//!
//! Almost every instance names the file it draws itself from, so the tree is mostly a way into the
//! models, shared groups and effects a zone is built out of.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use egui::{RichText, ScrollArea, Sense, Vec2, collapsing_header::paint_default_icon, vec2};
use ironworks::file::layer::{
    Colour, HelperKind, Instance, InstanceData, LayerGroup, Rgba, Scene, TriggerBox,
};
use ironworks::file::{lgb::LayerGroupFile, lvb::LevelFile, sgb::SharedGroupFile};

use super::{facts, link, section};
use crate::assets::deps::Deps;
use crate::backend::Backend;

pub mod lgb;
pub mod lvb;
pub mod scene;
pub mod sgb;
pub mod sound;

/// Space each level of the tree is set in by.
/// The sheet a scene filter names a territory of.
const TERRITORY: &str = "TerritoryType";

/// The sheets an instance names a row of. An event NPC is filed under its base id in the sheet
/// carrying the name, and an object under the one beside its own.
const RESIDENT: &str = "ENpcResident";
const OBJECT: &str = "EObjName";
const PLACE: &str = "PlaceName";
const MAP: &str = "Map";
const MUSIC: &str = "BGM";

/// The sheet it names a duty of, where the territory is entered through one.
const DUTY: &str = "ContentFinderCondition";

const INDENT: f32 = 12.0;

/// Room the expander takes, kept on rows without one so their labels still line up.
const TRIANGLE: f32 = 12.0;

/// Points of a path or a range listed before the rest are left to the count.
const LISTED: usize = 8;

/// The file the tree was read from, which it keeps rather than copying every instance out: all a
/// row needs is where to find its own.
enum Source {
    Group(LayerGroupFile),
    Shared(SharedGroupFile),
    Level(LevelFile),
}

impl Source {
    fn groups(&self) -> &[LayerGroup] {
        match self {
            Self::Group(file) => std::slice::from_ref(file.group()),
            Self::Shared(file) => file.scene().layer_groups(),
            Self::Level(file) => file.scene().layer_groups(),
        }
    }

    fn scene(&self) -> Option<&Scene> {
        match self {
            Self::Group(_) => None,
            Self::Shared(file) => Some(file.scene()),
            Self::Level(file) => Some(file.scene()),
        }
    }
}

/// A scene for a level file, for a host outside this module's own tree view: a cutscene names the
/// level it plays in and wants the same view this module opens from a `.lvb` link.
pub fn level_scene(path: &str, file: LevelFile) -> scene::Scene {
    scene::Scene::new(path, &Source::Level(file))
}

/// The files a scene names. A field the scene left empty is dropped rather than listed blank.
fn files(scene: &Scene) -> Vec<(&'static str, String)> {
    let mut files: Vec<(&'static str, String)> = scene
        .layer_group_paths()
        .iter()
        .map(|path| ("图层组", path.clone()))
        .collect();
    files.push(("天空可见性", scene.sky_visibility_path().clone()));
    files.push(("光照剔除", scene.light_culling_path().clone()));
    for environment in scene.environments() {
        files.push(("环境", environment.asset_path().clone()));
        files.push(("环境声音", environment.sound_asset_path().clone()));
    }
    files.retain(|(_, path)| !path.is_empty());
    files
}

/// Width held for the header grid's name column. Fitted to the longest name currently in
/// `HEADER_NAMES` ("sky visibility path"); longer names added later will truncate.
const HEADER_NAME_WIDTH: f32 = 130.0;

/// What each slot of the scene header's general block is, where anything has established one. The
/// blanks are real: nothing has identified them yet, and the viewer shows their bytes rather than
/// pretending otherwise.
const HEADER_NAMES: [&str; 24] = [
    "flags",
    "bg path",
    "environment list",
    "environments",
    "sun tilt, degrees",
    "sky visibility path",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "light culling path",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
];

/// Where a row sits in the tree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    Group(usize),
    Layer(usize, usize),
    Instance(usize, usize, usize),
}

impl At {
    fn depth(self) -> usize {
        match self {
            Self::Group(..) => 0,
            Self::Layer(..) => 1,
            Self::Instance(..) => 2,
        }
    }
}

/// One row as it is drawn.
struct Line<'a> {
    label: String,
    /// Where the row sits and what its payload says, drawn weakly after the label.
    detail: String,
    /// The file the row names.
    asset: Option<&'a str>,
}

/// One field of the selected row. A path is drawn as a link, a sheet row as whatever the sheet
/// calls it, and anything else as text.
enum Fact {
    Text(String),
    Path(String),
    /// A sheet and a row of it, drawn as the row's name beside its id.
    Row(&'static str, u32),
    /// The same where the row's text is a file, which is drawn as a link.
    Asset(&'static str, u32),
}

#[derive(Default)]
struct Rows(Vec<(&'static str, Fact)>);

impl Rows {
    fn text(&mut self, label: &'static str, value: impl Into<String>) {
        self.0.push((label, Fact::Text(value.into())));
    }

    /// Dropped where the field is blank, which is how an unset path is written.
    fn path(&mut self, label: &'static str, path: &str) {
        if !path.is_empty() {
            self.0.push((label, Fact::Path(path.to_owned())));
        }
    }

    /// Dropped where the field is zero, which is how an unset row is written.
    fn row(&mut self, label: &'static str, sheet: &'static str, id: u32) {
        if id != 0 {
            self.0.push((label, Fact::Row(sheet, id)));
        }
    }

    fn asset(&mut self, label: &'static str, sheet: &'static str, id: u32) {
        if id != 0 {
            self.0.push((label, Fact::Asset(sheet, id)));
        }
    }
}

pub struct Rendered {
    path: String,
    identity: Vec<(&'static str, String)>,
    /// The files the scene names, each of which the browser can open.
    files: Vec<(&'static str, String)>,
    /// The scene header's general block, a slot at a time, so what is not named is still readable.
    header: Vec<u32>,
    /// The territories the scene is used from, and the duty each is entered through.
    filters: Vec<(u16, u16)>,
    source: Source,
    rows: Vec<At>,
    /// Instance kinds and how many of each.
    kinds: Vec<(String, usize)>,
    /// Where the open rows and the selected one are kept, since drawing takes the file by
    /// reference.
    state: egui::Id,
    /// Which of the tree, the scene or the flattened sound list is showing. The scene and the
    /// sound list each own fetches of their own, so they are built on the first switch rather than
    /// with the file.
    view: Cell<View>,
    scene: RefCell<Option<scene::Scene>>,
    sounds: RefCell<Option<sound::Sounds>>,
    /// Whether the scene view is offered at all. A `.lvb` opens with it off, since the Assets tab
    /// no longer places one; `show_scene` turns it on for the Zones tab, which does.
    scene_enabled: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Tree,
    Scene,
    Sounds,
}

fn axes(values: [f32; 3]) -> String {
    format!("{:.3}, {:.3}, {:.3}", values[0], values[1], values[2])
}

fn span(values: [f32; 2]) -> String {
    format!("{:.2} to {:.2}", values[0], values[1])
}

fn on(value: bool) -> &'static str {
    match value {
        true => "是",
        false => "否",
    }
}

fn color(colour: Colour) -> String {
    format!(
        "{}, {}, {}, {} x {:.2}",
        colour.red(),
        colour.green(),
        colour.blue(),
        colour.alpha(),
        colour.intensity()
    )
}

fn rgba(colour: Rgba) -> String {
    format!(
        "{}, {}, {}, {}",
        colour.red(),
        colour.green(),
        colour.blue(),
        colour.alpha()
    )
}

/// How much room a volume covers. Nothing in the payload of a range states its size; the instance
/// is a unit shape and its scale is what gives it one.
fn extent(scale: [f32; 3]) -> String {
    format!(
        "{:.1} x {:.1} x {:.1}",
        scale[0].abs(),
        scale[1].abs(),
        scale[2].abs()
    )
}

fn trigger(rows: &mut Rows, trigger: TriggerBox, scale: [f32; 3]) {
    rows.text("形状", format!("{:?}  {}", trigger.shape(), extent(scale)));
    rows.text("优先级", trigger.priority().to_string());
    rows.text("启用", on(trigger.enabled()));
}

fn listed(points: impl ExactSizeIterator<Item = String>) -> String {
    let count = points.len();
    let mut listed = points.take(LISTED).collect::<Vec<_>>().join("\n");
    if count > LISTED {
        listed.push_str(&format!("\n… 其余 {} 项", count - LISTED));
    }
    listed
}

/// The file an instance draws itself from, where its payload names one.
pub fn asset(data: &InstanceData) -> Option<&str> {
    let path: &str = match data {
        InstanceData::BgPart(part) => part.asset_path(),
        InstanceData::SharedGroup(group) => group.asset_path(),
        InstanceData::Vfx(vfx) => vfx.asset_path(),
        InstanceData::Sound(sound) => sound.asset_path(),
        InstanceData::EnvSpace(space) => space.asset_path(),
        InstanceData::Light(light) => light.texture_path(),
        InstanceData::Decal(decal) => decal.diffuse_path(),
        InstanceData::EnvLocation(location) => location.ambient_light_asset_path(),
        InstanceData::CollisionBox(collision) => collision.collision_asset_path(),
        InstanceData::HelperObject(helper) => helper
            .nested()
            .and_then(|nested| asset(nested.data()))
            .unwrap_or_default(),
        _ => "",
    };
    (!path.is_empty()).then_some(path)
}

/// The one line of a payload worth reading beside the file it names.
fn summary(instance: &Instance) -> String {
    let scale = instance.transform().scale();
    match instance.data() {
        InstanceData::None => String::new(),
        InstanceData::BgPart(part) => match part.visible() {
            true => format!("{:?}", part.collision()),
            false => format!("{:?}, 隐藏", part.collision()),
        },
        InstanceData::Light(light) => format!("{:?}, 范围 {:.1}", light.kind(), light.range()),
        InstanceData::Vfx(vfx) => match vfx.auto_play() {
            true => "自动播放".to_owned(),
            false => String::new(),
        },
        InstanceData::PositionMarker(marker) => format!("{:?}", marker.kind()),
        InstanceData::SharedGroup(group) => format!("{:?}", group.initial_door_state()),
        InstanceData::Sound(sound) => format!("{:?}", sound.kind()),
InstanceData::HelperObject(helper) => match helper.base_id() {
            0 => format!("{:?}", helper.kind()),
            base => format!("{:?}, 基准 {base}", helper.kind()),
        },
        InstanceData::EventNpc(npc) => format!("基准 {}", npc.character().object().base_id()),
        InstanceData::Character(character) => format!("基准 {}", character.object().base_id()),
        InstanceData::Aetheryte(aetheryte) => format!("基准 {}", aetheryte.object().base_id()),
        InstanceData::EnvSpace(space) => format!("{:?}", space.shape()),
        InstanceData::Treasure(treasure) => format!("基准 {}", treasure.object().base_id()),
        InstanceData::Weapon(weapon) => format!("模型 {}", weapon.model().pattern_id()),
        InstanceData::PopRange(pop) => {
            format!("{:?}, {} 个位置", pop.kind(), pop.positions().len())
        }
        InstanceData::ExitRange(exit) => format!("{:?}, 区域 {}", exit.kind(), exit.zone_id()),
        InstanceData::MapRange(range) => format!("地图 {}", range.map()),
        InstanceData::EventObject(object) => format!("基准 {}", object.object().base_id()),
        InstanceData::EnvLocation(_) => String::new(),
        InstanceData::EventRange(box_)
        | InstanceData::DoorRange(box_)
        | InstanceData::ClickableRange(box_) => format!("{:?}  {}", box_.shape(), extent(scale)),
        InstanceData::QuestMarker(marker) => format!("{:?}", marker.unknown()),
        InstanceData::CollisionBox(collision) => {
            format!("{:?}  {}", collision.trigger().shape(), extent(scale))
        }
        InstanceData::LineVfx(line) => format!("{:?}", line.style()),
        InstanceData::ClientPath(path) => format!("{} 个点", path.control_points().len()),
        InstanceData::TargetMarker(marker) => format!("{:?}", marker.kind()),
        InstanceData::ChairMarker(chair) => format!("{:?}", chair.kind()),
        InstanceData::PrefetchRange(range) => {
            format!("{:?}  {}", range.trigger().shape(), extent(scale))
        }
        InstanceData::FateRange(range) => {
            format!("{:?}  {}", range.trigger().shape(), extent(scale))
        }
        InstanceData::Decal(_) => String::new(),
        InstanceData::CullingBox(_) => extent(scale),
        InstanceData::Unknown(bytes) => format!("{} 字节未读取", bytes.len()),
    }
}

/// Everything a payload holds, for the panel that inspects one instance.
/// The surface a collision material word states, beside the word itself.
fn surface(word: u64) -> String {
    match ironworks::file::pcb::surface(word) {
        Some(held) => format!("{word:#018x}, {held}"),
        None => format!("{word:#018x}"),
    }
}

fn payload(instance: &Instance) -> Rows {
    let scale = instance.transform().scale();
    let mut rows = Rows::default();
    match instance.data() {
        InstanceData::None => {}
        InstanceData::BgPart(part) => {
            rows.path("模型", part.asset_path());
            rows.path("碰撞体", part.collision_asset_path());
            rows.text("碰撞模式", format!("{:?}", part.collision()));
            if part.collision_material_mask() != 0 {
                rows.text(
                    "碰撞遮罩",
                    format!("{:#018x}", part.collision_material_mask()),
                );
            }
            if part.collision_material_id() != 0 {
                rows.text("碰撞材质", surface(part.collision_material_id()));
            }
            rows.text("可见", on(part.visible()));
            rows.text(
                "世界阴影",
                format!("{:?}", part.world_light_shadow_mode()),
            );
            rows.text(
                "物体阴影",
                format!("{:?}", part.object_light_shadow_mode()),
            );
            rows.text("淡出距离", format!("{:.1}", part.fade_out_distance()));
            rows.text(
                "包围球",
                format!("{:.1}", part.bounding_sphere_size()),
            );
        }
        InstanceData::Light(light) => {
            rows.text("灯光类型", format!("{:?}", light.kind()));
            rows.text("点光源类型", format!("{:?}", light.point_light_kind()));
            rows.text("范围", format!("{:.2}", light.range()));
            rows.text("衰减", format!("{:.3}", light.attenuation()));
            rows.text(
                "锥体系数",
                format!("{:.3}", light.attenuation_cone_coefficient()),
            );
            rows.text("聚光角度", format!("{:.3}", light.spot_angle()));
            rows.text("颜色", color(light.colour()));
            rows.path("纹理", light.texture_path());
            rows.text("高光", on(light.specular_highlights()));
            rows.text("场景阴影", on(light.bg_part_shadows()));
            rows.text("角色阴影", on(light.character_shadows()));
        }
        InstanceData::Vfx(vfx) => {
            rows.path("特效", vfx.asset_path());
            rows.text("颜色", rgba(vfx.colour()));
            rows.text(
                "柔和粒子淡出",
                format!("{:.2}", vfx.soft_particle_fade_range()),
            );
            rows.text("自动播放", on(vfx.auto_play()));
            rows.text("无远裁剪", on(vfx.no_far_clip()));
            rows.text("近端淡出", span(vfx.fade_near()));
            rows.text("远端淡出", span(vfx.fade_far()));
            rows.text("Z 校正", format!("{:.3}", vfx.z_correct()));
        }
        InstanceData::PositionMarker(marker) => {
            rows.text("标记", format!("{:?}", marker.kind()));
            rows.text("注释", format!("{:#x}", marker.comment_en_offset()));
            rows.text("注释（日文）", format!("{:#x}", marker.comment_jp_offset()));
        }
        InstanceData::HelperObject(helper) => {
            rows.text("Stands for", format!("{:?}", helper.kind()));
            match helper.kind() {
                HelperKind::BattleNpc => rows.row("Base", "BNpcBase", helper.base_id()),
                _ => rows.row("Base", "ENpcBase", helper.base_id()),
            }
            if helper.object_id() != 0 {
                rows.text("Object", helper.object_id().to_string());
            }
            if helper.kind() == HelperKind::Weapon {
                let model = helper.weapon();
                rows.text(
                    "Weapon",
                    format!(
                        "{}, {}, {}",
                        model.skeleton_id(),
                        model.pattern_id(),
                        model.image_change_id()
                    ),
                );
            }
            if helper.height() != 0 {
                rows.text("Height", ((u32::from(helper.height()) - 1) * 25).to_string());
            }
            if let Some(placement) = helper.placement() {
                rows.text("Stands at", axes(placement.transform().translation()));
                rows.text("Placement flags", format!("{:#x}", placement.flags()));
            }
            if let Some(nested) = helper.nested() {
                rows.0.extend(payload(nested).0);
            }
        }
        InstanceData::SharedGroup(group) => {
            rows.path("组", group.asset_path());
            rows.text("门", format!("{:?}", group.initial_door_state()));
            rows.text("旋转", format!("{:?}", group.initial_rotation_state()));
            rows.text(
                "变换",
                format!("{:?}", group.initial_transform_state()),
            );
            rows.text("颜色", format!("{:?}", group.initial_colour_state()));
            rows.text(
                "随机时间轴",
                format!(
                    "自动播放 {}，循环 {}",
                    on(group.random_timeline_auto_play()),
                    on(group.random_timeline_loop_playback())
                ),
            );
            rows.text(
                "无事件对象碰撞",
                on(group.collision_controllable_without_event_object()),
            );
            if group.bound_client_path_instance_id() != 0 {
                rows.text(
                    "绑定路径",
                    group.bound_client_path_instance_id().to_string(),
                );
            }
            let path = group.move_path();
            rows.text("移动路径", format!("{:?}", path.mode()));
            rows.text(
                "移动时机",
                format!(
                    "{}，时长 {}，加速 {}，减速 {}",
                    on(path.auto_play()),
                    path.time(),
                    path.accelerate_time(),
                    path.decelerate_time()
                ),
            );
            rows.text(
                "移动旋转",
                format!(
                    "{:?}，循环 {}，反向 {}",
                    path.rotation(),
                    on(path.loop_playback()),
                    on(path.reverse())
                ),
            );
            rows.text("垂直摆动", span(path.vertical_swing_range()));
            rows.text("水平摆动", span(path.horizontal_swing_range()));
            rows.text("摆动速度", span(path.swing_move_speed_range()));
            rows.text("摆动旋转", span(path.swing_rotation()));
            rows.text(
                "摆动旋转速度",
                span(path.swing_rotation_speed_range()),
            );
            if !group.overrides().is_empty() {
                rows.text(
                    "覆盖项",
                    format!("{} 字节未读取", group.overrides().len()),
                );
            }
        }
        InstanceData::Sound(sound) => {
            rows.path("声音", sound.asset_path());
            rows.text("发射器", format!("{:?}", sound.kind()));
            rows.text("自动播放", on(sound.auto_play()));
            rows.text("无远裁剪", on(sound.no_far_clip()));
            rows.text("点选择", sound.point_selection().to_string());
            if !sound.binary().is_empty() {
                rows.text("几何数据", format!("{} 字节未读取", sound.binary().len()));
            }
        }
        InstanceData::EventNpc(npc) => {
            rows.row("基准", RESIDENT, npc.character().object().base_id());
            rows.text("角色", format!("{:?}", npc.character().unknown()));
            rows.text("未知", format!("{:?}", npc.unknown()));
        }
        InstanceData::Character(character) => {
            rows.text("基准", character.object().base_id().to_string());
            rows.text("未知", format!("{:?}", character.unknown()));
        }
        InstanceData::Aetheryte(aetheryte) => {
            rows.text("基准", aetheryte.object().base_id().to_string());
            rows.text("绑定实例", aetheryte.bound_instance_id().to_string());
            rows.text("未知", aetheryte.unknown().to_string());
        }
        InstanceData::EnvSpace(space) => {
            rows.path("环境", space.asset_path());
            rows.path("声音", space.sound_asset_path());
            rows.text("形状", format!("{:?}  {}", space.shape(), extent(scale)));
            rows.text("绑定实例", space.bound_instance_id().to_string());
            rows.text("环境贴图拍摄点", on(space.env_map_shooting_point()));
            rows.text("优先级", space.priority().to_string());
            rows.text("有效范围", format!("{:.2}", space.effective_range()));
            rows.text("插值", space.interpolation_time().to_string());
            rows.text("混响", format!("{:.2}", space.reverb()));
            rows.text("滤波器", format!("{:.2}", space.filter()));
        }
        InstanceData::Treasure(treasure) => {
            rows.text("基准", treasure.object().base_id().to_string());
        }
        InstanceData::Weapon(weapon) => {
            let model = weapon.model();
            rows.text("骨架", model.skeleton_id().to_string());
            rows.text("图案", model.pattern_id().to_string());
            rows.text("贴图替换", model.image_change_id().to_string());
            rows.text("染色", model.staining_id().to_string());
            rows.text("可见", on(weapon.visible()));
        }
        InstanceData::PopRange(pop) => {
            rows.text("刷新类型", format!("{:?}", pop.kind()));
            rows.text(
                "内半径比例",
                format!("{:.3}", pop.inner_radius_ratio()),
            );
            rows.text("半径", extent(scale));
            if !pop.positions().is_empty() {
                rows.text(
                    "位置",
                    listed(pop.positions().iter().map(|point| axes(*point))),
                );
            }
        }
        InstanceData::ExitRange(exit) => {
            trigger(&mut rows, exit.trigger(), scale);
            rows.text("出口", format!("{:?}", exit.kind()));
            rows.text("区域", exit.zone_id().to_string());
            rows.text("区域类型", exit.territory_type_id().to_string());
            rows.text("索引", exit.index().to_string());
            rows.text(
                "目标实例",
                exit.destination_instance_id().to_string(),
            );
            rows.text("返回实例", exit.return_instance_id().to_string());
            rows.text(
                "奔跑方向",
                format!("{:.3}", exit.player_running_direction()),
            );
        }
        InstanceData::MapRange(range) => {
            trigger(&mut rows, range.trigger(), scale);
            rows.row("地图", MAP, range.map());
            rows.row("地名", PLACE, range.place_name_block());
            rows.row("地名牌", PLACE, range.place_name_spot());
            rows.text("天气", range.weather().to_string());
            rows.asset("音乐", MUSIC, range.bgm());
            rows.text("房屋地块", range.housing_block_id().to_string());
            rows.text("发现点", range.discovery_id().to_string());
            let switches = [
                ("地图", range.map_enabled()),
                ("地名", range.place_name_enabled()),
                ("发现点", range.discovery_enabled()),
                ("音乐", range.bgm_enabled()),
                ("仅进入时播放音乐", range.bgm_play_zone_in_only()),
                ("天气", range.weather_enabled()),
                ("休息奖励", range.rest_bonus_enabled()),
                ("休息奖励生效", range.rest_bonus_effective()),
                ("升降台", range.lift_enabled()),
                ("房屋", range.housing_enabled()),
                ("记录飞行高度误差", range.log_flying_height_max_err()),
                ("禁用坐骑", range.mounts_and_ornaments_disabled()),
                ("仅拉拉菲尔", range.lalafell_only()),
            ];
            let on = switches
                .iter()
                .filter(|(_, set)| *set)
                .map(|(name, _)| *name)
                .collect::<Vec<_>>();
            rows.text(
                "已启用",
                match on.is_empty() {
                    true => "无".to_owned(),
                    false => on.join(", "),
                },
            );
        }
        InstanceData::EventObject(object) => {
            rows.row("基准", OBJECT, object.object().base_id());
            rows.text("绑定实例", object.bound_instance_id().to_string());
            rows.text("未知", object.unknown().to_string());
        }
        InstanceData::EnvLocation(location) => {
            rows.path("环境光", location.ambient_light_asset_path());
            rows.path("环境贴图", location.env_map_asset_path());
        }
        InstanceData::EventRange(box_)
        | InstanceData::DoorRange(box_)
        | InstanceData::ClickableRange(box_) => trigger(&mut rows, *box_, scale),
        InstanceData::QuestMarker(marker) => {
            rows.text("未知", format!("{:?}", marker.unknown()));
        }
        InstanceData::CollisionBox(collision) => {
            trigger(&mut rows, collision.trigger(), scale);
            rows.path("碰撞体", collision.collision_asset_path());
            rows.text(
                "碰撞遮罩",
                format!("{:#018x}", collision.collision_material_mask()),
            );
            rows.text(
                "碰撞材质",
                surface(collision.collision_material_id()),
            );
        }
        InstanceData::LineVfx(line) => rows.text("样式", format!("{:?}", line.style())),
        InstanceData::ClientPath(path) => {
            rows.text("点数", path.control_points().len().to_string());
            rows.text(
                "控制点",
                listed(path.control_points().iter().map(|point| {
                    format!(
                        "{}  id {}{}",
                        axes(point.position()),
                        point.id(),
                        match point.select() {
                            true => "，已选",
                            false => "",
                        }
                    )
                })),
            );
        }
        InstanceData::TargetMarker(marker) => {
            rows.text("锚点", format!("{:?}", marker.kind()));
            rows.text(
                "名牌偏移",
                format!("{:.3}", marker.nameplate_offset_y()),
            );
        }
        InstanceData::ChairMarker(chair) => {
            rows.text("座位", format!("{:?}", chair.kind()));
            let sides = [
                ("左", chair.left()),
                ("右", chair.right()),
                ("后", chair.back()),
            ];
            let taken = sides
                .iter()
                .filter(|(_, set)| *set)
                .map(|(name, _)| *name)
                .collect::<Vec<_>>();
            rows.text(
                "座位方向",
                match taken.is_empty() {
                    true => "无".to_owned(),
                    false => taken.join(", "),
                },
            );
        }
        InstanceData::PrefetchRange(range) => {
            trigger(&mut rows, range.trigger(), scale);
            rows.text("绑定实例", range.bound_instance_id().to_string());
        }
        InstanceData::FateRange(range) => {
            trigger(&mut rows, range.trigger(), scale);
            rows.text(
                "FATE 布局标签",
                range.fate_layout_label_id().to_string(),
            );
        }
        InstanceData::Decal(decal) => {
            rows.path("漫反射", decal.diffuse_path());
            rows.path("法线", decal.normal_path());
            rows.path("高光", decal.specular_path());
        }
        InstanceData::CullingBox(box_) => {
            rows.text("体积", extent(scale));
            rows.text("未知", box_.unknown().to_string());
        }
        InstanceData::Unknown(bytes) => rows.text(
            "载荷",
            format!("{:?} 未读取，{} 字节", instance.kind(), bytes.len()),
        ),
    }
    rows
}

fn rows(groups: &[LayerGroup]) -> Vec<At> {
    let mut rows = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        rows.push(At::Group(group_index));
        for (layer_index, layer) in group.layers().iter().enumerate() {
            rows.push(At::Layer(group_index, layer_index));
            rows.extend(
                (0..layer.instances().len())
                    .map(|instance| At::Instance(group_index, layer_index, instance)),
            );
        }
    }
    rows
}

fn tally(groups: &[LayerGroup]) -> Vec<(String, usize)> {
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for instance in groups
        .iter()
        .flat_map(LayerGroup::layers)
        .flat_map(|layer| layer.instances())
    {
        let name = format!("{:?}", instance.kind());
        match kinds.iter_mut().find(|(kind, _)| *kind == name) {
            Some((_, count)) => *count += 1,
            None => kinds.push((name, 1)),
        }
    }
    kinds.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    kinds
}

fn rendered(path: &str, mut identity: Vec<(&'static str, String)>, source: Source) -> Rendered {
    let scene_enabled = !matches!(source, Source::Level(_));
    let rows = rows(source.groups());
    let kinds = tally(source.groups());
    let instances = kinds.iter().map(|(_, count)| count).sum::<usize>();
    match (source.groups().is_empty(), source.scene()) {
        // A level or shared group can name its layer groups by path instead of embedding them, so
        // the file itself has no layer or instance count to give until those are read.
        (true, Some(scene)) => identity.push((
            "图层组",
            format!("命名 {} 个，未嵌入", scene.layer_group_paths().len()),
        )),
        _ => {
            identity.push((
                "图层",
                source
                    .groups()
                    .iter()
                    .map(|group| group.layers().len())
                    .sum::<usize>()
                    .to_string(),
            ));
            identity.push(("实例", instances.to_string()));
        }
    }

    log::info!("assets/layer: {path}，{instances} 个实例");

    Rendered {
        path: path.to_owned(),
        identity,
        files: source.scene().map(files).unwrap_or_default(),
        header: source.scene().map(|held| held.general().to_vec()).unwrap_or_default(),
        filters: source
            .scene()
            .map(|scene| {
                scene
                    .filters()
                    .iter()
                    .map(|filter| (filter.territory_type(), filter.content_finder_condition()))
                    .collect()
            })
            .unwrap_or_default(),
        source,
        rows,
        kinds,
        state: egui::Id::new(path).with("layer_tree"),
        view: Cell::new(View::Tree),
        scene: RefCell::new(None),
        sounds: RefCell::new(None),
        scene_enabled: Cell::new(scene_enabled),
    }
}

pub fn ui(
    ui: &mut egui::Ui,
    file: &Rendered,
    deps: &mut Deps,
    backend: &Backend,
) -> Option<String> {
    ui.horizontal(|ui| {
        if ui
            .selectable_label(file.view.get() == View::Tree, "树状")
            .clicked()
        {
            file.view.set(View::Tree);
        }
        if file.scene_enabled.get()
            && ui
                .selectable_label(file.view.get() == View::Scene, "场景")
                .clicked()
        {
            file.view.set(View::Scene);
        }
        if ui
            .selectable_label(file.view.get() == View::Sounds, "声音")
            .clicked()
        {
            file.view.set(View::Sounds);
        }
    });
    ui.add_space(4.0);

    match file.view.get() {
        View::Scene => {
            let mut held = file.scene.borrow_mut();
            scene::ui(
                ui,
                held.get_or_insert_with(|| scene::Scene::new(&file.path, &file.source)),
                backend,
            );
            return None;
        }
        View::Sounds => {
            let mut held = file.sounds.borrow_mut();
            return sound::ui(
                ui,
                held.get_or_insert_with(|| sound::Sounds::new(&file.source)),
                backend,
            );
        }
        View::Tree => {}
    }

    let mut follow = None;
    if !file.files.is_empty() {
        section(ui, "文件");
        egui::Grid::new("layer_files")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (label, path) in &file.files {
                    ui.label(RichText::new(*label).weak());
                    if link(ui, crate::utils::file_name(path), path) {
                        follow = Some(path.clone());
                    }
                    ui.allocate_space(vec2(ui.available_width(), 0.0));
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        ui.separator();
    }

    if !file.header.is_empty() {
        section(ui, "场景头");
        egui::Grid::new("layer_header")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for (slot, held) in file.header.iter().enumerate() {
                    ui.label(RichText::new(format!("+{:#06x}", slot * 4)).weak());
                    ui.label(RichText::new(held.to_string()).monospace());
                    // Both readings, since the block mixes offsets and counts with distances and
                    // angles, and only some of them are named yet.
                    let held = f32::from_bits(*held);
                    ui.label(
                        RichText::new(match held.is_finite() && held.abs() < 1e9 {
                            true => format!("{held:.3}"),
                            false => String::new(),
                        })
                        .monospace(),
                    );
                    // Fixed rather than natural: an untracked name column is this grid's widest
                    // cell forever, and it never shrinks back down once measured wide.
                    ui.add_sized(
                        vec2(HEADER_NAME_WIDTH, 0.0),
                        egui::Label::new(
                            RichText::new(HEADER_NAMES.get(slot).copied().unwrap_or("")).weak(),
                        )
                        .truncate(),
                    );
                    ui.allocate_space(vec2(ui.available_width(), 0.0));
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        ui.separator();
    }

    if !file.filters.is_empty() {
        section(ui, "使用于");
        egui::Grid::new("layer_filters")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for &(territory, duty) in &file.filters {
                    ui.label(RichText::new(format!("区域 {territory}")).weak());
                    let named = deps.text(ui.ctx(), backend, TERRITORY, u32::from(territory));
                    ui.label(RichText::new(named.unwrap_or_default()).monospace());
                    if duty > 0 {
                        // The duty's leading text is an internal code; its name follows.
                        let named = deps.text_at(ui.ctx(), backend, DUTY, 1, u32::from(duty));
                        ui.label(
                            RichText::new(named.map_or_else(
                                || format!("副本 {duty}"),
                                |name| format!("{name} ({duty})"),
                            ))
                            .monospace(),
                        );
                    }
                    ui.allocate_space(vec2(ui.available_width(), 0.0));
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        ui.separator();
    }
    // A level names its layer groups rather than holding any, so there is no tree to draw for one.
    if file.rows.is_empty() {
        return follow;
    }

    let mut open = file.open(ui);
    let mut shown = Vec::new();
    let mut collapsed_at = None;
    for (index, at) in file.rows.iter().enumerate() {
        match collapsed_at {
            Some(depth) if at.depth() > depth => continue,
            _ => collapsed_at = None,
        }
        let parent = file.parent(*at);
        // Only a group is open on arrival, since which layer a thing sits on is most of what the
        // file says.
        let expanded = parent && ((at.depth() == 0) != open.contains(&index));
        if parent && !expanded {
            collapsed_at = Some(at.depth());
        }
        shown.push((index, expanded));
    }

    section(ui, "图层");
    let picked = file.selected(ui);
    let mut selected = picked;
    let mut toggled = None;
    // A selectable label pads itself, so the height the scroll area is told has to leave room for
    // that. `show_rows` adds the spacing between rows on top of it.
    let height = ui
        .text_style_height(&egui::TextStyle::Monospace)
        .max(TRIANGLE)
        + 2.0 * ui.spacing().button_padding.y;
    ScrollArea::vertical()
        .auto_shrink(false)
        .show_rows(ui, height, shown.len(), |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            for &(index, expanded) in &shown[range] {
                let at = file.rows[index];
                let line = file.line(at);
                ui.horizontal(|ui| {
                    ui.add_space(at.depth() as f32 * INDENT);
                    match file.parent(at) {
                        false => ui.add_space(TRIANGLE),
                        true => {
                            let (_, response) =
                                ui.allocate_exact_size(Vec2::splat(TRIANGLE), Sense::click());
                            let openness = match expanded {
                                true => 1.0,
                                false => 0.0,
                            };
                            paint_default_icon(ui, openness, &response);
                            if response.clicked() {
                                toggled = Some(index);
                            }
                        }
                    }

                    if ui
                        .selectable_label(
                            picked == Some(index),
                            RichText::new(&line.label).monospace(),
                        )
                        .clicked()
                    {
                        selected = Some(index);
                    }
                    if let Some(asset) = line.asset
                        && link(ui, crate::utils::file_name(asset), asset)
                    {
                        follow = Some(asset.to_owned());
                    }
                    if !line.detail.is_empty() {
                        ui.label(RichText::new(&line.detail).monospace().weak());
                    }
                });
            }
        });

    if selected != picked {
        ui.data_mut(|data| data.insert_temp(file.state.with("selected"), selected));
    }
    if let Some(index) = toggled {
        if !open.insert(index) {
            open.remove(&index);
        }
        ui.data_mut(|data| data.insert_temp(file.state, open));
    }
    follow
}

impl Rendered {
    /// Lands a fresh view on the placed scene rather than the raw tree, for a host built
    /// specifically to show it. Also what turns the scene view on in the first place for a `.lvb`.
    pub fn show_scene(&self) {
        self.scene_enabled.set(true);
        self.view.set(View::Scene);
    }

    fn open(&self, ui: &egui::Ui) -> HashSet<usize> {
        ui.data(|data| data.get_temp(self.state).unwrap_or_default())
    }

    fn selected(&self, ui: &egui::Ui) -> Option<usize> {
        ui.data(|data| data.get_temp(self.state.with("selected")).flatten())
    }

    /// Whether anything sits under a row, which the walk over the whole tree asks of every one of
    /// them and so builds nothing.
    fn parent(&self, at: At) -> bool {
        let groups = self.source.groups();
        match at {
            At::Group(group) => !groups[group].layers().is_empty(),
            At::Layer(group, layer) => !groups[group].layers()[layer].instances().is_empty(),
            At::Instance(..) => false,
        }
    }

    fn instance(&self, at: At) -> Option<&Instance> {
        match at {
            At::Instance(group, layer, instance) => {
                Some(&self.source.groups()[group].layers()[layer].instances()[instance])
            }
            _ => None,
        }
    }

    fn line(&self, at: At) -> Line<'_> {
        let groups = self.source.groups();
        match at {
            At::Group(group) => {
                let group = &groups[group];
                Line {
                    label: match group.name().is_empty() {
                        true => group.id().to_string(),
                        false => group.name().clone(),
                    },
                    detail: format!("{} 个图层", group.layers().len()),
                    asset: None,
                }
            }
            At::Layer(group, layer) => {
                let layer = &groups[group].layers()[layer];
                Line {
                    label: layer.name().clone(),
                    detail: format!("{} 个实例", layer.instances().len()),
                    asset: None,
                }
            }
            At::Instance(group, layer, instance) => {
                let instance = &groups[group].layers()[layer].instances()[instance];
                let transform = instance.transform();
                let mut detail = format!("位于 {}", axes(transform.translation()));
                if transform.rotation() != [0.0; 3] {
                    detail.push_str(&format!("  旋转 {}", axes(transform.rotation())));
                }
                if transform.scale() != [1.0; 3] {
                    detail.push_str(&format!("  缩放 {}", axes(transform.scale())));
                }
                let summary = summary(instance);
                if !summary.is_empty() {
                    detail.push_str("  ");
                    detail.push_str(&summary);
                }
                Line {
                    label: match instance.name().is_empty() {
                        true => format!("{:?} {}", instance.kind(), instance.id()),
                        false => {
                            format!(
                                "{:?} {} {}",
                                instance.kind(),
                                instance.id(),
                                instance.name()
                            )
                        }
                    },
                    detail,
                    asset: asset(instance.data()),
                }
            }
        }
    }

    /// Everything the selected row carries.
    fn fields(&self, at: At) -> Vec<(&'static str, Fact)> {
        let groups = self.source.groups();
        let mut rows = Rows::default();
        match at {
            At::Group(group) => {
                let group = &groups[group];
                rows.text("组", group.id().to_string());
                rows.text("图层", group.layers().len().to_string());
            }
            At::Layer(group, layer) => {
                let layer = &groups[group].layers()[layer];
                rows.text("图层", layer.id().to_string());
                rows.text("实例", layer.instances().len().to_string());
                rows.text("可见", on(layer.visible()));
                if layer.festival_id() != 0 {
                    rows.text(
                        "庆典",
                        format!(
                            "{} 阶段 {}",
                            layer.festival_id(),
                            layer.festival_phase_id()
                        ),
                    );
                }
            }
            At::Instance(..) => {
                let instance = self.instance(at).expect("an instance row");
                let transform = instance.transform();
                rows.text("实例", instance.id().to_string());
                rows.text("位置", axes(transform.translation()));
                rows.text("旋转", axes(transform.rotation()));
                rows.text("缩放", axes(transform.scale()));
                rows.0.extend(payload(instance).0);
            }
        }
        rows.0
    }

    pub fn details_ui(
        &self,
        ui: &mut egui::Ui,
        follow: &mut Option<String>,
        deps: &mut Deps,
        backend: &Backend,
    ) {
        if self.view.get() == View::Scene
            && let Some(scene) = self.scene.borrow_mut().as_mut()
        {
            scene.details_ui(ui, follow, deps, backend);
            return;
        }
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if let Some(index) = self.selected(ui)
                && let Some(&at) = self.rows.get(index)
            {
                ui.label(RichText::new(self.line(at).label).strong());
                ui.add_space(4.0);
                egui::Grid::new("layer_selected")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (label, fact) in self.fields(at) {
                            ui.label(RichText::new(label).weak());
                            match fact {
                                Fact::Text(value) => {
                                    ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
                                }
                                Fact::Row(sheet, id) => {
                                    let named = deps.text(ui.ctx(), backend, sheet, id);
                                    ui.label(
                                        RichText::new(match named {
                                            Some(name) => format!("{name}  ({id})"),
                                            None => id.to_string(),
                                        })
                                        .monospace(),
                                    );
                                }
                                Fact::Asset(sheet, id) => {
                                    match deps.text(ui.ctx(), backend, sheet, id) {
                                        Some(path) => {
                                            let path = path.to_owned();
                                            if link(ui, crate::utils::file_name(&path), &path) {
                                                *follow = Some(path);
                                            }
                                        }
                                        None => {
                                            ui.label(RichText::new(id.to_string()).monospace());
                                        }
                                    }
                                }
                                Fact::Path(path) => {
                                    if link(ui, crate::utils::file_name(&path), &path) {
                                        *follow = Some(path);
                                    }
                                }
                            }
                            ui.allocate_space(vec2(ui.available_width(), 0.0));
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.separator();
            }

            facts(ui, "layer_identity", &self.identity);
            if self.kinds.is_empty() {
                return;
            }
            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("实例类型").weak());
            ui.add_space(4.0);
            egui::Grid::new("layer_kinds")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for (kind, count) in &self.kinds {
                        ui.label(RichText::new(kind).monospace());
                        ui.label(RichText::new(count.to_string()).monospace());
                        ui.allocate_space(vec2(ui.available_width(), 0.0));
                        ui.end_row();
                    }
                });
        });
    }
}
