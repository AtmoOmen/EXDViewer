//! `.waoe`: which models have an attach-offset file at all.
//!
//! A `u16` count then that many sorted `u16` ids. The client tests membership before it builds a
//! path, so a model the list omits is never fetched: `sub_1404501A0` asks the list for a monster's
//! own id and for a demihuman's **plus 10,000**, and asks nothing at all for a character, whose
//! `c%04d.atch` it loads unconditionally. Measured against the install, every one of the 51
//! `m*.atch` ids is listed as itself and 67 of the 68 `d*.atch` ids as id + 10,000.

use anyhow::{Context, Result};
use egui::RichText;

use super::{Preview, facts, line, section, table};

/// Where a demihuman's own id starts, which is what keeps the two kinds apart in one list.
const DEMIHUMAN: u16 = 10000;

const COLUMNS: [(&str, usize); 3] = [("ID", 8), ("类型", 12), ("读取的文件", 0)];

pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    rows: Vec<u16>,
}

/// The ids a list states, in the order it states them.
pub fn ids(bytes: &[u8]) -> Option<Vec<u16>> {
    let count = usize::from(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?));
    (0..count)
        .map(|at| {
            let held = bytes.get(2 + at * 2..4 + at * 2)?;
            Some(u16::from_le_bytes(held.try_into().ok()?))
        })
        .collect()
}

/// The file an id stands for: a demihuman above the mark, a monster below it.
pub fn named(id: u16) -> (&'static str, String) {
    match id >= DEMIHUMAN {
        true => ("亚人", format!("d{:04}.atch", id - DEMIHUMAN)),
        false => ("魔物", format!("m{id:04}.atch")),
    }
}

/// Whether a model of one kind states an attach offset of its own.
pub fn holds(ids: &[u16], id: u16, demihuman: bool) -> bool {
    let wanted = match demihuman {
        true => id.saturating_add(DEMIHUMAN),
        false => id,
    };
    ids.binary_search(&wanted).is_ok()
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let rows = ids(bytes).context("附着偏移列表")?;
    let demihumans = rows.iter().filter(|id| **id >= DEMIHUMAN).count();
    let identity = vec![
        ("模型", rows.len().to_string()),
        ("魔物", (rows.len() - demihumans).to_string()),
        ("亚人", demihumans.to_string()),
    ];
    log::info!("assets/waoe: {path} {} 个模型", rows.len());
    Ok(Preview::Waoe(Box::new(Rendered { identity, rows })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    section(ui, "带附着偏移的模型");
    ui.label(
        RichText::new(
            "表中没有的模型不会被查询。亚人记录为自身 id 加一万；角色完全不出现在表中，\
             无论这里写了什么，其文件都会被读取。",
        )
        .weak(),
    );
    ui.add_space(4.0);
    table(ui, &COLUMNS, file.rows.len(), |ui, index| {
        let id = file.rows[index];
        let (kind, reads) = named(id);
        ui.label(
            RichText::new(line(&COLUMNS, [id.to_string().as_str(), kind, &reads])).monospace(),
        );
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        facts(ui, "waoe_identity", &self.identity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A count then that many ids, and nothing at all out of anything shorter than it claims.
    #[test]
    fn a_list_states_its_own_length() {
        assert_eq!(ids(&[2, 0, 12, 0, 30, 0]), Some(vec![12, 30]));
        assert_eq!(ids(&[3, 0, 12, 0]), None);
        assert_eq!(ids(&[]), None);
    }

    /// The two kinds share one list, and ten thousand is what tells them apart.
    #[test]
    fn a_demihuman_is_stated_ten_thousand_above_itself() {
        assert_eq!(named(361), ("魔物", "m0361.atch".to_owned()));
        assert_eq!(named(11006), ("亚人", "d1006.atch".to_owned()));
        let held = [361, 11006];
        assert!(holds(&held, 361, false));
        assert!(holds(&held, 1006, true));
        assert!(!holds(&held, 1006, false));
        assert!(!holds(&held, 361, true));
    }
}
