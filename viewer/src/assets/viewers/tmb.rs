//! `.tmb` timelines: what a character does over one animation, as commands grouped into tracks and
//! tracks into actors.
//!
//! The file lists its items flat and nests them by id, so the tree is rebuilt from the ids each item
//! names. The same items are embedded in `.pap`, which draws them through [`Items`].

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea};
use ironworks::file::File;
use ironworks::file::tmb::{Command, CommandKind, Condition, Item, Timeline};

use super::{Preview, facts, link, section};
use crate::utils::file_name;

/// Space each level of the tree is set in by.
const INDENT: f32 = 12.0;

macro_rules! commands {
    ($($magic:ident)*) => {
        /// A command's magic and the body the crate models under it.
        fn described(kind: &CommandKind) -> (String, String) {
            match kind {
                $(CommandKind::$magic(body) => (
                    stringify!($magic).to_owned(),
                    format!("{body:?}"),
                ),)*
                CommandKind::Unknown { magic, body } => (
                    String::from_utf8_lossy(magic).into_owned(),
                    format!("{} bytes unread", body.len()),
                ),
            }
        }
    };
}

commands!(
    C002 C004 C006 C009 C010 C011 C012 C013 C014 C015 C018 C019 C021 C031 C033 C034 C040 C042
    C043 C048 C049 C053 C055 C056 C057 C058 C059 C063 C067 C068 C075 C082 C083 C084 C088 C089
    C090 C093
    C094 C095 C100 C104 C107 C109 C110 C112 C113 C117 C118 C120 C124 C125 C131 C133 C136 C139
    C142 C143 C144 C161 C168 C173 C174 C175 C176 C177 C178 C187 C188 C192 C194 C197 C198 C199
    C202 C203 C204 C211 C212 C215 C216 C225 C230 C234
);

/// The asset a command plays, for the kinds that name one.
///
/// `C009`, `C010`, `C040` and `C090` also carry a `motion()`, but it names an animation inside
/// whatever `.pap` the cutscene's own `CTRL` resources list, not a file of its own; the body text
/// below still shows it.
fn asset(command: &Command) -> Option<&str> {
    match command.kind() {
        CommandKind::C002(play) => play.path(),
        CommandKind::C012(effect) => effect.path(),
        CommandKind::C049(effect) => effect.path(),
        CommandKind::C063(sound) => sound.path(),
        CommandKind::C173(effect) => effect.path(),
        _ => None,
    }
}

/// The ids an item nests under itself.
fn children(item: &Item) -> &[i16] {
    match item {
        Item::ActorList(list) => list.actors(),
        Item::Actor(actor) => actor.tracks(),
        Item::Track(track) => track.commands(),
        _ => &[],
    }
}

/// One drawn line: an item, or a step of the condition gating a track.
struct Row {
    item: usize,
    step: Option<Condition>,
    depth: usize,
}

/// A timeline's items, in the order the ids nest them. The timeline itself stays with whatever holds
/// it, since a `.cutb` keeps dozens of them inside the one file they were read from.
pub struct Items {
    rows: Vec<Row>,
}

impl Items {
    pub fn new(timeline: &Timeline) -> Self {
        let items = timeline.items();
        let mut by_id = HashMap::new();
        for (index, item) in items.iter().enumerate() {
            if let Some(id) = item.id() {
                by_id.entry(id).or_insert(index);
            }
        }
        let nested = items
            .iter()
            .flat_map(children)
            .filter_map(|id| by_id.get(id).copied())
            .collect::<HashSet<_>>();

        let mut rows = Vec::new();
        let mut drawn = vec![false; items.len()];
        for index in 0..items.len() {
            if !nested.contains(&index) {
                push(items, &by_id, &mut rows, &mut drawn, index, 0);
            }
        }
        // An id two items claim, or one that names itself, is left over by the walk above.
        for index in 0..items.len() {
            if !drawn[index] {
                push(items, &by_id, &mut rows, &mut drawn, index, 0);
            }
        }

        Self { rows }
    }

    pub fn ui(&self, ui: &mut egui::Ui, timeline: &Timeline) -> Option<String> {
        let mut follow = None;
        let items = timeline.items();
        for row in &self.rows {
            let item = &items[row.item];
            ui.horizontal(|ui| {
                ui.add_space(row.depth as f32 * INDENT);
                match row.step {
                    Some(step) => {
                        ui.label(
                            RichText::new(format!(
                                "operation {:#04x}  value {:#010x}  {}",
                                step.operation(),
                                step.value(),
                                step.float()
                            ))
                            .monospace()
                            .weak(),
                        );
                    }
                    None => {
                        ui.label(RichText::new(head(item)).monospace());
                        if let Some(path) = path(item)
                            && link(ui, file_name(path), path)
                        {
                            follow = Some(path.to_owned());
                        }
                        let body = body(item);
                        if !body.is_empty() {
                            ui.label(RichText::new(body).monospace().weak());
                        }
                    }
                }
            });
        }
        follow
    }
}

/// Adds an item and everything nested under it.
fn push(
    items: &[Item],
    by_id: &HashMap<i16, usize>,
    rows: &mut Vec<Row>,
    drawn: &mut [bool],
    index: usize,
    depth: usize,
) {
    if drawn[index] {
        return;
    }
    drawn[index] = true;
    rows.push(Row {
        item: index,
        step: None,
        depth,
    });

    if let Item::Track(track) = &items[index] {
        for step in track.condition() {
            rows.push(Row {
                item: index,
                step: Some(*step),
                depth: depth + 1,
            });
        }
    }

    for id in children(&items[index]) {
        if let Some(child) = by_id.get(id) {
            push(items, by_id, rows, drawn, *child, depth + 1);
        }
    }
}

fn magic(item: &Item) -> String {
    match item {
        Item::Header(_) => "TMDH".to_owned(),
        Item::FaceLibrary(_) => "TMPP".to_owned(),
        Item::ActorList(_) => "TMAL".to_owned(),
        Item::Actor(_) => "TMAC".to_owned(),
        Item::Track(_) => "TMTR".to_owned(),
        Item::Curves(_) => "TMFC".to_owned(),
        Item::Command(command) => described(command.kind()).0,
        Item::Unknown(unknown) => String::from_utf8_lossy(&unknown.magic()).into_owned(),
    }
}

/// The magic, the id items reference it by, and when it runs.
fn head(item: &Item) -> String {
    let id = item.id().map_or(String::new(), |id| format!("#{id}"));
    let time = match item {
        Item::Actor(actor) => Some(actor.time()),
        Item::Track(track) => Some(track.time()),
        Item::Curves(curves) => Some(curves.time()),
        Item::Command(command) => Some(command.time()),
        _ => None,
    }
    .map_or(String::new(), |time| format!("@{time}"));
    format!("{:<5}{id:>7} {time:>7}  ", magic(item))
}

fn path(item: &Item) -> Option<&str> {
    match item {
        Item::FaceLibrary(library) => library.path(),
        Item::Command(command) => asset(command),
        _ => None,
    }
}

fn body(item: &Item) -> String {
    match item {
        Item::Header(header) => format!(
            "duration {}, unknown {}, {}",
            header.duration(),
            header.unknown_1(),
            header.unknown_3()
        ),
        Item::FaceLibrary(_) => String::new(),
        Item::ActorList(list) => format!("{} actors", list.actors().len()),
        Item::Actor(actor) => format!(
            "{} tracks, ability delay {}, participant {:#x}",
            actor.tracks().len(),
            actor.ability_delay(),
            actor.participant()
        ),
        Item::Track(track) => format!("{} commands", track.commands().len()),
        Item::Curves(curves) => format!(
            "{} curves over {} targets, end {}, unknown {}",
            curves.curves().len(),
            curves.targets(),
            curves.end(),
            curves.unknown_b()
        ),
        Item::Command(command) => described(command.kind()).1,
        Item::Unknown(unknown) => format!("{} bytes unread", unknown.body().len()),
    }
}

/// A timeline, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    timeline: Timeline,
    items: Items,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let timeline = Timeline::read(Cursor::new(bytes.to_vec()))?;
    let items = Items::new(&timeline);

    log::info!("assets/tmb: {path} {} items", timeline.items().len());

    Ok(Preview::Tmb(Box::new(Rendered {
        identity: identity(&timeline),
        timeline,
        items,
    })))
}

/// What the timeline holds, for the details panel. The same rows sit under a `.pap`'s animations.
pub fn identity(timeline: &Timeline) -> Vec<(&'static str, String)> {
    let items = timeline.items();
    let mut identity = vec![("Items", items.len().to_string())];
    if let Some(duration) = items.iter().find_map(|item| match item {
        Item::Header(header) => Some(header.duration()),
        _ => None,
    }) {
        identity.push(("Duration", duration.to_string()));
    }

    let mut counts: Vec<(String, usize)> = Vec::new();
    for item in items {
        let magic = magic(item);
        match counts.iter_mut().find(|(name, _)| *name == magic) {
            Some((_, count)) => *count += 1,
            None => counts.push((magic, 1)),
        }
    }
    identity.push((
        "Kinds",
        counts
            .iter()
            .map(|(magic, count)| format!("{magic} {count}"))
            .collect::<Vec<_>>()
            .join(", "),
    ));
    identity
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) -> Option<String> {
    let mut follow = None;
    section(ui, "Items");
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        follow = file.items.ui(ui, &file.timeline);
    });
    follow
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "tmb_identity", &self.identity));
    }
}

#[cfg(test)]
mod tests {
    use super::{Items, Timeline};
    use ironworks::file::File;

    /// A timeline of one actor holding one track holding one command, with the id lists in a pool
    /// past the items. Offsets are relative to the item's own start plus its eight byte header.
    fn timeline() -> Vec<u8> {
        let (actors, tracks, commands) = (116u32, 118u32, 120u32);
        let mut bytes = Vec::new();
        bytes.extend(b"TMLB");
        bytes.extend(122u32.to_le_bytes());
        bytes.extend(5u32.to_le_bytes());

        bytes.extend(b"TMDH");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(0i16.to_le_bytes());
        bytes.extend(0i16.to_le_bytes());
        bytes.extend(100i16.to_le_bytes());
        bytes.extend(3i16.to_le_bytes());

        bytes.extend(b"TMAL");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend((actors - 36).to_le_bytes());
        bytes.extend(1u32.to_le_bytes());

        bytes.extend(b"TMAC");
        bytes.extend(28u32.to_le_bytes());
        bytes.extend(1i16.to_le_bytes());
        bytes.extend(0i16.to_le_bytes());
        bytes.extend(0i32.to_le_bytes());
        bytes.extend(0i32.to_le_bytes());
        bytes.extend((tracks - 52).to_le_bytes());
        bytes.extend(1u32.to_le_bytes());

        bytes.extend(b"TMTR");
        bytes.extend(24u32.to_le_bytes());
        bytes.extend(2i16.to_le_bytes());
        bytes.extend(0i16.to_le_bytes());
        bytes.extend((commands - 80).to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(0i32.to_le_bytes());

        bytes.extend(b"C011");
        bytes.extend(20u32.to_le_bytes());
        bytes.extend(3i16.to_le_bytes());
        bytes.extend(5i16.to_le_bytes());
        bytes.extend(1i32.to_le_bytes());
        bytes.extend(0i32.to_le_bytes());

        bytes.extend(1u16.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        bytes
    }

    /// The file lists its items flat, and only the ids say which of them run under which.
    #[test]
    fn nests_the_items_the_ids_name() {
        let read = Timeline::read(std::io::Cursor::new(timeline())).unwrap();
        let items = Items::new(&read);

        assert_eq!(read.items().len(), 5);
        assert_eq!(
            super::identity(&read)[1],
            ("Duration", "100".to_owned()),
            "the header's duration"
        );
        assert_eq!(
            items
                .rows
                .iter()
                .map(|row| (row.item, row.depth))
                .collect::<Vec<_>>(),
            [(0, 0), (1, 0), (2, 1), (3, 2), (4, 3)]
        );
    }
}
