//! What a zone annotates its layers with: how much sky reaches an instance, the box a light is
//! clipped against, and the settings the zone takes underwater.
//!
//! `.svb` and `.lcb` key every entry by an instance one of the zone's layer groups placed. `.uwb`
//! shares their container and nothing else: one group holds the whole of how a zone looks under
//! water.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::{File, lcb, svb, uwb};

use super::super::{Preview, facts, line, link, placed, section, table};
use super::axes;
use crate::utils::file_name;

/// Prepended to the columns below where a file holds more than one group, since a single group has
/// nothing to tell its rows apart from.
const GROUP: (&str, usize) = ("分组", 5);

const VISIBILITY: [(&str, usize); 3] = [("实例", 10), ("成员", 9), ("可见性", 10)];

const CLIP: [(&str, usize); 4] = [("实例", 10), ("成员", 9), ("最小", 26), ("最大", 26)];

const UNDERWATER: [(&str, usize); 2] = [("设置", 34), ("值", 12)];

/// Everything one underwater group sets, in the order the format writes it.
fn settings(group: uwb::Group) -> Vec<(String, String)> {
    let fog = |depth: &str, fog: uwb::Fog| {
        [
            (format!("{depth} 雾渐隐上界"), fog.vertical_fade_upper()),
            (format!("{depth} 雾渐隐下界"), fog.vertical_fade_lower()),
            (
                format!("{depth} 雾衰减"),
                fog.vertical_attenuation_strength(),
            ),
        ]
    };

    let scalars = [
        ("水面 Y".to_owned(), group.water_surface_y()),
        (
            "深度过渡起点".to_owned(),
            group.depth_transition_start(),
        ),
        (
            "深度过渡范围".to_owned(),
            group.depth_transition_range(),
        ),
    ]
    .into_iter()
    .chain(fog("浅水", group.fog_shallow()))
    .chain(fog("深水", group.fog_deep()))
    .chain([
        (
            "焦散渐隐起点".to_owned(),
            group.caustics_distance_fade_start(),
        ),
        (
            "焦散渐隐范围".to_owned(),
            group.caustics_distance_fade_range(),
        ),
        ("焦散 UV 尺寸 1".to_owned(), group.caustics_uv_size()[0]),
        ("焦散 UV 尺寸 2".to_owned(), group.caustics_uv_size()[1]),
        (
            "焦散滚动速度".to_owned(),
            group.caustics_scroll_speed(),
        ),
        ("焦散强度".to_owned(), group.caustics_intensity()),
        ("太阳大小".to_owned(), group.sun_size()),
        ("太阳渐隐起点".to_owned(), group.sun_fade_start()),
        (
            "光照倍率".to_owned(),
            group.lighting_multiplier(),
        ),
    ])
    .map(|(name, value)| (name, format!("{value:.3}")));

    std::iter::once(("版本".to_owned(), group.version().to_string()))
        .chain(scalars)
        .chain(std::iter::once((
            "未知".to_owned(),
            group.unknown().to_string(),
        )))
        .collect()
}

/// The file the table was read from, which it keeps rather than copying every entry out.
enum Source {
    Sky(svb::SkyVisibility),
    Clip(lcb::ClipBoxes),
    Underwater(uwb::Underwater),
}

impl Source {
    /// How many rows each group contributes.
    fn lengths(&self) -> Vec<usize> {
        match self {
            Self::Sky(file) => file.groups().iter().map(|g| g.entries().len()).collect(),
            Self::Clip(file) => file.groups().iter().map(|g| g.entries().len()).collect(),
            Self::Underwater(file) => file.groups().iter().map(|g| settings(*g).len()).collect(),
        }
    }

    fn cells(&self, group: usize, index: usize) -> Vec<String> {
        match self {
            Self::Sky(file) => {
                let entry = file.groups()[group].entries()[index];
                vec![
                    entry.instance().to_string(),
                    member(entry.members()),
                    format!("{:.3}", entry.visibility()),
                ]
            }
            Self::Clip(file) => {
                let entry = file.groups()[group].entries()[index];
                vec![
                    entry.instance().to_string(),
                    member(entry.members()),
                    axes(entry.min()),
                    axes(entry.max()),
                ]
            }
            Self::Underwater(file) => {
                let (name, value) = settings(file.groups()[group])[index].clone();
                vec![name, value]
            }
        }
    }

    /// Instances the file keys entries by, each counted once.
    fn instances(&self) -> Option<usize> {
        let keys: HashSet<u32> = match self {
            Self::Sky(file) => file
                .groups()
                .iter()
                .flat_map(|group| group.entries())
                .map(|entry| entry.instance())
                .collect(),
            Self::Clip(file) => file
                .groups()
                .iter()
                .flat_map(|group| group.entries())
                .map(|entry| entry.instance())
                .collect(),
            Self::Underwater(_) => return None,
        };
        Some(keys.len())
    }

    /// The version every group of the file declares, where they agree on one.
    fn version(&self) -> Option<i32> {
        let mut versions = match self {
            Self::Sky(file) => file
                .groups()
                .iter()
                .map(svb::Group::version)
                .collect::<Vec<_>>(),
            Self::Clip(file) => file.groups().iter().map(lcb::Group::version).collect(),
            Self::Underwater(file) => file.groups().iter().map(uwb::Group::version).collect(),
        }
        .into_iter();
        let first = versions.next()?;
        versions.all(|it| it == first).then_some(first)
    }

    /// How much sky the entries let through, which is the whole of what a `.svb` says.
    fn spread(&self) -> Option<(f32, f32, usize)> {
        let Self::Sky(file) = self else {
            return None;
        };
        let visibility = file
            .groups()
            .iter()
            .flat_map(|group| group.entries())
            .map(|entry| entry.visibility());
        let mut count = 0;
        let mut open = 0;
        let mut total = 0.0;
        let mut least = f32::INFINITY;
        for value in visibility {
            count += 1;
            total += value;
            least = least.min(value);
            open += usize::from(value >= 1.0);
        }
        (count > 0).then(|| (least, total / count as f32, open))
    }
}

/// Reaches the part of an instance an entry applies to, an index per level of shared group it sits
/// under. The format fills them from the front, so the run before the first zero is the whole path.
fn member(members: [u8; 4]) -> String {
    members
        .iter()
        .take_while(|&&index| index != 0)
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    /// The clip boxes in space, built on the first switch to the scene.
    scene: RefCell<Option<placed::View>>,
    /// Whether the table or the scene is showing, for the file that has one.
    placed: Cell<bool>,
    /// The zone's scene, which names the layer groups the instances were placed in.
    level: String,
    section: &'static str,
    columns: Vec<(&'static str, usize)>,
    source: Source,
    /// Group and index of every row.
    rows: Vec<(usize, usize)>,
}

fn rendered(
    path: &str,
    section: &'static str,
    columns: &[(&'static str, usize)],
    source: Source,
) -> Preview {
    let lengths = source.lengths();
    let rows = lengths
        .iter()
        .enumerate()
        .flat_map(|(group, count)| (0..*count).map(move |index| (group, index)))
        .collect::<Vec<_>>();
    let columns = match lengths.len() > 1 {
        true => std::iter::once(GROUP)
            .chain(columns.iter().copied())
            .collect(),
        false => columns.to_vec(),
    };

    let mut identity = Vec::new();
    if let Some(version) = source.version() {
        identity.push(("版本", version.to_string()));
    }
    identity.push(("分组", lengths.len().to_string()));
    if let Some(instances) = source.instances() {
        identity.push(("条目", rows.len().to_string()));
        identity.push(("实例", instances.to_string()));
    }
    if let Some((least, mean, open)) = source.spread() {
        identity.push(("最低可见性", format!("{least:.3}")));
        identity.push(("平均可见性", format!("{mean:.3}")));
        identity.push(("完全开放", open.to_string()));
    }

    log::info!("assets/zone: {path} {} 行", rows.len());

    Preview::Zone(Box::new(Rendered {
        identity,
        scene: RefCell::new(None),
        placed: Cell::new(false),
        level: format!(
            "{}.lvb",
            path.rsplit_once('.').map_or(path, |(stem, _)| stem)
        ),
        section,
        columns,
        source,
        rows,
    }))
}

pub fn sky_visibility(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = svb::SkyVisibility::read(Cursor::new(bytes.to_vec()))?;
    Ok(rendered(path, "可见性", &VISIBILITY, Source::Sky(file)))
}

pub fn clip_boxes(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = lcb::ClipBoxes::read(Cursor::new(bytes.to_vec()))?;
    Ok(rendered(path, "裁剪盒", &CLIP, Source::Clip(file)))
}

pub fn underwater(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = uwb::Underwater::read(Cursor::new(bytes.to_vec()))?;
    Ok(rendered(
        path,
        "值",
        &UNDERWATER,
        Source::Underwater(file),
    ))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) -> Option<String> {
    let mut follow = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("关卡").weak());
        if link(ui, file_name(&file.level), &file.level) {
            follow = Some(file.level.clone());
        }
        if matches!(file.source, Source::Clip(_)) {
            ui.separator();
            for (scene, label) in [(false, "表格"), (true, "场景")] {
                if ui
                    .selectable_label(file.placed.get() == scene, label)
                    .clicked()
                {
                    file.placed.set(scene);
                }
            }
        }
    });
    ui.add_space(4.0);

    if file.placed.get() {
        let mut held = file.scene.borrow_mut();
        held.get_or_insert_with(|| file.build()).ui(ui);
        return follow;
    }

    section(ui, file.section);
    let grouped = file.columns.first() == Some(&GROUP);
    table(ui, &file.columns, file.rows.len(), |ui, index| {
        let (group, entry) = file.rows[index];
        let mut cells = file.source.cells(group, entry);
        if grouped {
            cells.insert(0, group.to_string());
        }
        ui.label(RichText::new(line(&file.columns, cells.iter().map(String::as_str))).monospace());
    });
    follow
}

impl Rendered {
    /// Every clip box as the volume it bounds, drawn as edges since they overlap and a solid one
    /// would hide the rest.
    fn build(&self) -> placed::View {
        let Source::Clip(file) = &self.source else {
            return placed::View::new(Vec::new());
        };
        let instances = file
            .groups()
            .iter()
            .flat_map(|group| group.entries())
            .enumerate()
            .map(|(index, entry)| {
                let (min, max) = (entry.min(), entry.max());
                placed::Instance {
                    center: std::array::from_fn(|axis| (min[axis] + max[axis]) * 0.5),
                    scale: std::array::from_fn(|axis| (max[axis] - min[axis]) * 0.5),
                    turn: [0.0, 0.0, 0.0, 1.0],
                    color: placed::tint(index),
                }
            })
            .collect();
        placed::View::new(vec![placed::Batch {
            shape: placed::Shape::Wire,
            instances,
        }])
    }

    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "zone_identity", &self.identity));
    }
}
