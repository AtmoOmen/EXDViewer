//! `.cutb` cutscenes: the nodes the file is built from, the files it loads, and a timeline per shot.
//!
//! The timelines hold the same commands `.tmb` does, and most of what a cutscene puts in them is a
//! kind that crate does not model, so a row saying only its magic and its size is the honest one.

mod play;

use std::cell::Cell;
use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::cutb::{Cutscene, Node};
use ironworks::file::tmb::{CommandKind, Item};

use super::{Preview, facts, line, link, section, table, tmb as timeline};
use crate::backend::Backend;

const NODES: [(&str, usize); 3] = [("Node", 6), ("Kind", 6), ("Holds", 60)];
const RESOURCES: [(&str, usize); 2] = [("Flag", 6), ("File", 8)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Files,
    Timelines,
    Nodes,
    Play,
}

/// A cutscene, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    file: Cutscene,
    /// Each node's magic and what it holds, in the order the file lists them.
    nodes: Vec<(String, String)>,
    /// Every file the cutscene loads, and the flag beside it.
    resources: Vec<(String, u32)>,
    /// The node each timeline came from, and the tree its items nest into.
    timelines: Vec<(usize, timeline::Items)>,
    tab: Cell<Tab>,
    /// Which timeline is on screen, indexing [`Self::timelines`].
    shown: Cell<usize>,
    play: play::Tab,
}

fn magic(node: &Node) -> String {
    match node {
        Node::Resources(_) => "CTRL".to_owned(),
        Node::Sheet(_) => "CTIS".to_owned(),
        Node::Scene(_) => "CTDS".to_owned(),
        Node::Participants(_) => "CTAL".to_owned(),
        Node::Groups(_) => "CTPA".to_owned(),
        Node::Tracks(_) => "CTEX".to_owned(),
        Node::Timeline(_) => "CTTL".to_owned(),
        Node::Unknown(unknown) => String::from_utf8_lossy(&unknown.magic()).into_owned(),
    }
}

fn holds(node: &Node) -> String {
    match node {
        Node::Resources(list) => format!("{} files", list.len()),
        Node::Sheet(sheet) => format!("sheet {sheet}"),
        Node::Scene(scene) => format!("{}, {} entries", scene.level(), scene.entries().len()),
        Node::Participants(participants) => format!(
            "{} participants: {}",
            participants.len(),
            play::roll_call(participants)
        ),
        Node::Groups(groups) => format!(
            "{} groups, {} records",
            groups.len(),
            groups
                .iter()
                .map(|group| group.records().len())
                .sum::<usize>()
        ),
        Node::Tracks(tracks) => format!(
            "{} runs, {} values",
            tracks.len(),
            tracks
                .iter()
                .map(|track| track.values().len())
                .sum::<usize>()
        ),
        Node::Timeline(timeline) => format!("{} items", timeline.items().len()),
        Node::Unknown(unknown) => format!("{} bytes unread", unknown.body().len()),
    }
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = Cutscene::read(Cursor::new(bytes.to_vec()))?;

    let mut nodes = Vec::with_capacity(file.nodes().len());
    let mut resources = Vec::new();
    let mut timelines = Vec::new();
    let mut level = String::new();
    let mut origin = String::new();
    let mut sheet = String::new();
    let mut shots = 0;
    for (index, node) in file.nodes().iter().enumerate() {
        match node {
            Node::Resources(list) => resources.extend(
                list.iter()
                    .map(|resource| (resource.path().to_owned(), resource.unknown_1())),
            ),
            Node::Sheet(named) => sheet = named.clone(),
            Node::Scene(scene) => {
                level = scene.level().to_owned();
                let [x, y, z] = scene.origin();
                origin = format!("{x:.2}, {y:.2}, {z:.2}");
            }
            Node::Timeline(found) => {
                shots += found
                    .items()
                    .iter()
                    .filter(|item| {
                        matches!(item, Item::Command(command)
                            if matches!(command.kind(), CommandKind::C004(_)))
                    })
                    .count();
                timelines.push((index, timeline::Items::new(found)));
            }
            _ => {}
        }
        nodes.push((magic(node), holds(node)));
    }

    let items = file
        .nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Timeline(found) => Some(found.items().len()),
            _ => None,
        })
        .sum::<usize>();

    let play = play::Tab::new(level.clone(), &file);
    let identity = vec![
        ("Nodes", nodes.len().to_string()),
        ("Level", level),
        ("Origin", origin),
        ("Sheet", sheet),
        ("Shots", shots.to_string()),
        ("Files", resources.len().to_string()),
        ("Timelines", timelines.len().to_string()),
        ("Items", items.to_string()),
    ];

    log::info!(
        "assets/cutb: {path} {} nodes, {} files, {} timelines",
        nodes.len(),
        resources.len(),
        timelines.len()
    );

    Ok(Preview::Cutb(Box::new(Rendered {
        identity,
        file,
        nodes,
        resources,
        timelines,
        tab: Cell::new(Tab::Files),
        shown: Cell::new(0),
        play,
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered, backend: &Backend) -> Option<String> {
    let mut follow = None;
    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Files, "Files"),
            (Tab::Timelines, "Timelines"),
            (Tab::Nodes, "Nodes"),
            (Tab::Play, "Play"),
        ] {
            if ui.selectable_label(file.tab.get() == tab, label).clicked() {
                file.tab.set(tab);
            }
        }
    });
    ui.add_space(4.0);

    match file.tab.get() {
        Tab::Nodes => {
            section(ui, "Nodes");
            table(ui, &NODES, file.nodes.len(), |ui, index| {
                let (magic, holds) = &file.nodes[index];
                let cells = [index.to_string(), magic.clone(), holds.clone()];
                ui.label(RichText::new(line(&NODES, cells.iter().map(String::as_str))).monospace());
            });
        }
        Tab::Files => {
            section(ui, "Files");
            table(ui, &RESOURCES, file.resources.len(), |ui, index| {
                let (path, flag) = &file.resources[index];
                let head = line(&RESOURCES, [flag.to_string().as_str()]);
                ui.horizontal(|ui| {
                    // The link is a widget of its own where the rest of the row is one padded
                    // string, so the spacing between them has to go for it to land under its header.
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(RichText::new(head).monospace());
                    if link(ui, crate::utils::file_name(path), path) {
                        follow = Some(path.clone());
                    }
                });
            });
        }
        Tab::Timelines => follow = file.timelines_ui(ui),
        Tab::Play => follow = play::ui(ui, &file.play, &file.file, backend),
    }
    follow
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "cutb_identity", &self.identity));
    }

    /// One timeline at a time: a cutscene holds up to a hundred of them, and an item list is drawn
    /// row by row rather than virtualised.
    fn timelines_ui(&self, ui: &mut egui::Ui) -> Option<String> {
        if self.timelines.is_empty() {
            ui.label(RichText::new("This cutscene holds no timelines").weak());
            return None;
        }
        let shown = self.shown.get().min(self.timelines.len() - 1);
        ScrollArea::horizontal()
            .id_salt("cutb_timelines")
            .max_height(ui.text_style_height(&egui::TextStyle::Button) + 8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, (node, _)) in self.timelines.iter().enumerate() {
                        if ui
                            .selectable_label(index == shown, format!("Node {node}"))
                            .clicked()
                        {
                            self.shown.set(index);
                        }
                    }
                });
            });
        ui.add_space(4.0);

        let (node, items) = &self.timelines[shown];
        let Some(Node::Timeline(timeline)) = self.file.nodes().get(*node) else {
            return None;
        };
        let mut follow = None;
        ScrollArea::both()
            .id_salt("cutb_items")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                follow = items.ui(ui, timeline);
            });
        follow
    }
}
