//! `.dic` word dictionaries: the lists the vulgar word filter matches chat against.

use std::{
    cell::{Ref, RefCell},
    io::Cursor,
};

use anyhow::Result;
use egui::{RichText, ScrollArea, TextEdit};
use ironworks::file::{File, dic};

use super::{Preview, facts, line, section, table};
use crate::utils::FuzzyMatcher;

const COLUMNS: [(&str, usize); 2] = [("列表", 7), ("词语", 0)];

/// A dictionary, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    /// Which list a word came from, and the word.
    rows: Vec<(&'static str, String)>,
    dropped: String,
    folded: String,
    query: egui::Id,
    matcher: FuzzyMatcher,
    matched: RefCell<(String, Vec<usize>)>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = dic::WordDictionary::read(Cursor::new(bytes.to_vec()))?;

    let rows = file
        .lists()
        .iter()
        .flat_map(|list| {
            let name = if list.blocked() { "已禁用" } else { "已允许" };
            list.words().iter().map(move |word| (name, word.clone()))
        })
        .collect::<Vec<_>>();

    let count = |blocked| rows.iter().filter(|(name, _)| *name == blocked).count();
    let identity = vec![
        ("列表", file.lists().len().to_string()),
        ("词语", rows.len().to_string()),
        ("已禁用", count("已禁用").to_string()),
        ("已允许", count("已允许").to_string()),
        ("已剔除", file.skipped().len().to_string()),
        ("已归并", file.replacements().len().to_string()),
    ];

    log::info!(
        "assets/dic: {path} {} lists, {} words",
        file.lists().len(),
        rows.len()
    );

    Ok(Preview::Dic(Box::new(Rendered {
        identity,
        rows,
        dropped: file
            .skipped()
            .iter()
            .map(|point| format!("{point} "))
            .collect(),
        folded: file
            .replacements()
            .iter()
            .map(|(from, to)| format!("{from}  {to}\n"))
            .collect(),
        query: egui::Id::new(("dic search", path)),
        matcher: FuzzyMatcher::new(),
        matched: RefCell::new((String::new(), Vec::new())),
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    section(ui, "词语");

    let mut query = ui
        .data(|data| data.get_temp::<String>(file.query))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.add(
            TextEdit::singleline(&mut query)
                .hint_text("搜索词语")
                .desired_width(240.0),
        );
        let matched = file.matched(&query);
        ui.label(
            RichText::new(format!("{} / {} 个词语", matched.len(), file.rows.len())).weak(),
        );
    });
    ui.data_mut(|data| data.insert_temp(file.query, query.clone()));
    ui.add_space(4.0);

    let matched = file.matched(&query);
    table(ui, &COLUMNS, matched.len(), |ui, index| {
        let (list, word) = &file.rows[matched[index]];
        ui.label(RichText::new(line(&COLUMNS, [*list, word.as_str()])).monospace());
    });
}

impl Rendered {
    /// Matching every word again each frame costs more than the file did to read, so the rows the
    /// last query left are kept.
    fn matched(&self, query: &str) -> Ref<'_, Vec<usize>> {
        if self.matched.borrow().0 != query {
            let matched = self.matcher.match_list_indirect(
                (!query.is_empty()).then_some(query),
                self.rows
                    .iter()
                    .enumerate()
                    .map(|(index, (_, word))| (index, word.as_str())),
                |item| item.1,
            );
            let rows = matched.into_iter().map(|(index, _)| index).collect();
            *self.matched.borrow_mut() = (query.to_owned(), rows);
        }
        Ref::map(self.matched.borrow(), |(_, rows)| rows)
    }

    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            facts(ui, "dic_identity", &self.identity);

            if !self.dropped.is_empty() {
                section(ui, "已剔除");
                ui.label(
                    RichText::new("匹配前从词组中剔除的字。" ).weak(),
                );
                ui.label(RichText::new(&self.dropped).monospace());
            }

            if !self.folded.is_empty() {
                section(ui, "已归并");
                ui.label(RichText::new("匹配前按其他字形读取的字。" ).weak());
                ui.label(RichText::new(&self.folded).monospace());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One character map, one list, and a node spelling `ab` and `ac` a character at a time.
    fn dictionary() -> Vec<u8> {
        let words = |points: &[u16]| -> Vec<u8> {
            points
                .iter()
                .flat_map(|point| point.to_le_bytes())
                .collect()
        };

        let mut bytes = vec![0u8; 0x8124];
        bytes[0x800c] = 1;
        bytes[0x810c..0x8110].copy_from_slice(&1u32.to_le_bytes());
        bytes[0x8110..0x8114].copy_from_slice(&0x8324u32.to_le_bytes());
        bytes.extend(words(&(0..0x100).collect::<Vec<_>>()));

        let mut begin = vec![0u16; 0x200];
        begin[0x100 + 0x61] = 1;
        let tables = [
            words(&begin),
            Vec::new(),
            words(&[0x62, 0x63]),
            Vec::new(),
            [0, 0, 0, 0, 0, 2, 0, 0]
                .iter()
                .flat_map(|field: &u32| field.to_le_bytes())
                .collect(),
        ];

        let mut at = 0u32;
        for table in &tables {
            bytes.extend(at.to_le_bytes());
            at += table.len() as u32;
        }
        for table in &tables {
            bytes.extend((table.len() as u32).to_le_bytes());
        }
        bytes.extend(1u32.to_le_bytes());

        let mut blocks = vec![0u16; 0x100];
        blocks[0] = 1;
        bytes.extend(words(&blocks));
        bytes.extend(tables.concat());
        bytes
    }

    #[test]
    fn searches_the_words_it_read() {
        let Preview::Dic(file) = decode("common/test.dic", &dictionary()).unwrap() else {
            panic!("decoded as something else");
        };

        assert_eq!(
            file.rows,
            [("已禁用", "ab".to_owned()), ("已禁用", "ac".to_owned())]
        );
        assert_eq!(*file.matched("ac"), [1usize]);
        assert_eq!(*file.matched(""), [0usize, 1]);

        egui::__run_test_ui(|test| ui(test, &file));
    }
}
