//! What a body stands in: the pose the weapon it holds puts it in, the motions it draws and
//! sheathes with, and how long the game takes to blend one motion into another.
//!
//! A weapon's motion class is its model set's own entry in `chara/xls/weapontype/motion.wtd`, a
//! sorted table of set to three-letter code the client binary-searches and clamps to the last
//! entry at or below the set it asks for. Main hand and off hand each read one, and the pair names
//! the directory the drawn packs are filed in: `bt_swd_sld`, `bt_2ax_emp`, and so on, the same
//! spelling `bt_%c%c%c_%c%c%c` builds. Empty hands read `emp`, and `bt_emp_emp`'s own idle pack
//! holds no animation at all, which is the game stating that bare hands have no drawn pose.
//!
//! Blend lengths are the game's own: `MotionTimeline` gives each motion name a blend group and
//! `MotionTimelineBlendTable` gives every ordered pair of groups a frame count, which the client
//! reads at a floor of one frame and 30 frames a second. A pair the table does not state falls
//! back to the one out of group 0, then to the one into group 0, then to 0 to 0.

use std::collections::HashMap;

use anyhow::{Context, Result};
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelSheet as _};

/// The animation set a player character's own packs are filed under.
const SET: u16 = 1;

/// The table naming which motion class each weapon model set is in.
const MOTION_TYPES: &str = "chara/xls/weapontype/motion.wtd";

/// What both hands read as with nothing in them.
const EMPTY: &str = "emp";

/// The directory every body's own unarmed packs are filed in, whatever it holds.
const COMMON: &str = "bt_common";

/// The motion a body stands in sheathed and drawn, and the partial motions it draws and sheathes
/// with, which are the same names in every class's own packs.
pub const SHEATHED: &str = "cbnm_id0";
pub const DRAWN: &str = "cbbm_id0";
pub const DRAW: &str = "cbbp_a_activ";
pub const SHEATHE: &str = "cbbp_a_deact";

/// Frames a second the blend table counts in.
const FPS: f32 = 30.0;

/// `MotionTimeline`'s filename and blend group, and `MotionTimelineBlendTable`'s destination
/// group, source group and player-character frame count, as byte offsets.
const FILENAME: u32 = 0;
const GROUP: u32 = 4;
const DEST: u32 = 0;
const SOURCE: u32 = 1;
const FRAMES: u32 = 2;

/// Where a body's animation packs are filed, which every class directory hangs off.
pub fn root(code: u16) -> String {
    format!("chara/human/c{code:04}/animation/a{SET:04}")
}

/// The pack a class's own `file` is filed as, for the class directory `held` names.
pub fn pack(code: u16, held: &str, file: &str) -> String {
    format!("{}/{held}/{file}", root(code))
}

/// The idle pack every body plays with nothing drawn.
pub fn sheathed_pack(code: u16) -> String {
    pack(code, COMMON, "resident/idle.pap")
}

/// Everything a change of stance is resolved out of: which class a weapon puts the body in, and
/// how long the game blends one motion into another.
pub struct Stance {
    /// Weapon model set to motion class, in the table's own ascending order.
    classes: Vec<(u32, String)>,
    /// Which blend group each motion's own name is in.
    groups: HashMap<String, u8>,
    /// Frames the blend from one group into another runs for.
    blends: HashMap<(u8, u8), u8>,
}

impl Stance {
    pub async fn read(backend: &Backend, language: Language) -> Result<Self> {
        let classes = weapon_types(&backend.files().read(MOTION_TYPES).await?)
            .context("weapon motion types")?;

        let excel = backend.excel();
        let motions = excel.get_sheet("MotionTimeline", language).await?;
        let mut groups = HashMap::new();
        for id in motions.get_row_ids() {
            let Ok(row) = motions.get_row(id) else {
                continue;
            };
            if let (Ok(name), Ok(group)) = (row.read_string(FILENAME), row.read::<u8>(GROUP))
                && !name.to_string().is_empty()
            {
                groups.insert(name.to_string(), group);
            }
        }

        let table = excel.get_sheet("MotionTimelineBlendTable", language).await?;
        let mut blends = HashMap::new();
        for id in table.get_row_ids() {
            let Ok(row) = table.get_row(id) else {
                continue;
            };
            if let (Ok(dest), Ok(source), Ok(frames)) = (
                row.read::<u8>(DEST),
                row.read::<u8>(SOURCE),
                row.read::<u8>(FRAMES),
            ) {
                blends.insert((source, dest), frames);
            }
        }
        log::info!(
            "character: {} weapon classes, {} blend groups, {} blends",
            classes.len(),
            groups.len(),
            blends.len()
        );
        Ok(Self {
            classes,
            groups,
            blends,
        })
    }

    /// The motion class a weapon model set is in: the last entry at or below it, since the table
    /// states one set per run of them and the client clamps a lookup between two into the lower.
    /// Nothing held is an empty hand.
    pub fn class(&self, set: Option<u16>) -> &str {
        let Some(set) = set else {
            return EMPTY;
        };
        let at = self
            .classes
            .partition_point(|(held, _)| *held <= u32::from(set));
        match self.classes.get(at.saturating_sub(1)) {
            Some((_, class)) => class,
            None => EMPTY,
        }
    }

    /// The directory a pair of weapons files its drawn packs under.
    pub fn directory(&self, main: Option<u16>, off: Option<u16>) -> String {
        format!("bt_{}_{}", self.class(main), self.class(off))
    }

    /// How long the game blends `from` into `to`, in seconds. A pair the table says nothing about
    /// takes the fallbacks the client's own lookup fills the matrix in with, and a stated zero
    /// still runs for a frame.
    pub fn fade(&self, from: &str, to: &str) -> f32 {
        let (Some(source), Some(dest)) = (self.group(from), self.group(to)) else {
            return 0.0;
        };
        let frames = [(source, dest), (0, dest), (source, 0), (0, 0)]
            .iter()
            .find_map(|pair| self.blends.get(pair))
            .copied()
            .unwrap_or_default();
        f32::from(frames.max(1)) / FPS
    }

    fn group(&self, motion: &str) -> Option<u8> {
        self.groups.get(motion).copied()
    }
}

/// The weapon type table: a four-byte header whose second half counts the entries, then one
/// four-byte model set and one three-letter code packed into four bytes for each.
fn weapon_types(bytes: &[u8]) -> Option<Vec<(u32, String)>> {
    let count = usize::from(u16::from_le_bytes(bytes.get(2..4)?.try_into().ok()?));
    (0..count)
        .map(|at| {
            let entry = bytes.get(4 + at * 8..12 + at * 8)?;
            let set = u32::from_le_bytes(entry[..4].try_into().ok()?);
            let code = String::from_utf8(entry[4..7].iter().rev().copied().collect()).ok()?;
            Some((set, code))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(u32, &str)]) -> Vec<u8> {
        let mut bytes = vec![1, 0];
        bytes.extend((entries.len() as u16).to_le_bytes());
        for (set, code) in entries {
            bytes.extend(set.to_le_bytes());
            bytes.extend([code.as_bytes()[2], code.as_bytes()[1], code.as_bytes()[0], 0]);
        }
        bytes
    }

    fn stance(entries: &[(u32, &str)]) -> Stance {
        Stance {
            classes: weapon_types(&table(entries)).expect("the table reads"),
            groups: HashMap::new(),
            blends: HashMap::new(),
        }
    }

    /// The three-letter codes `motion.wtd` itself carries for the first weapon sets it names.
    #[test]
    fn a_weapon_set_reads_the_class_the_table_files_it_under() {
        let held = stance(&[(101, "sld"), (201, "swd"), (301, "clw"), (401, "2ax")]);
        assert_eq!(held.class(Some(201)), "swd");
        assert_eq!(held.class(Some(250)), "swd", "between two entries reads the lower");
        assert_eq!(held.class(Some(401)), "2ax");
        assert_eq!(held.class(Some(50)), "sld", "below the first still reads one");
        assert_eq!(held.class(None), "emp");
    }

    #[test]
    fn a_pair_of_weapons_names_the_directory_their_packs_are_filed_in() {
        let held = stance(&[(101, "sld"), (201, "swd"), (401, "2ax")]);
        assert_eq!(held.directory(Some(201), Some(101)), "bt_swd_sld");
        assert_eq!(held.directory(Some(401), None), "bt_2ax_emp");
        assert_eq!(held.directory(None, None), "bt_emp_emp");
    }

    /// The blend table's own fallback order, which fills every unstated pair from the entries out
    /// of and into group 0 before the one that is both.
    #[test]
    fn an_unstated_blend_falls_back_the_way_the_client_fills_its_matrix() {
        let mut held = stance(&[]);
        held.groups.insert("from".to_owned(), 7);
        held.groups.insert("to".to_owned(), 6);
        held.blends.insert((0, 0), 3);
        assert_eq!(held.fade("from", "to"), 3.0 / FPS);
        held.blends.insert((7, 0), 9);
        assert_eq!(held.fade("from", "to"), 9.0 / FPS);
        held.blends.insert((0, 6), 4);
        assert_eq!(held.fade("from", "to"), 4.0 / FPS);
        held.blends.insert((7, 6), 12);
        assert_eq!(held.fade("from", "to"), 12.0 / FPS);
    }

    #[test]
    fn a_blend_of_no_frames_still_runs_for_one() {
        let mut held = stance(&[]);
        held.groups.insert("from".to_owned(), 9);
        held.groups.insert("to".to_owned(), 8);
        held.blends.insert((9, 8), 0);
        assert_eq!(held.fade("from", "to"), 1.0 / FPS);
        assert_eq!(held.fade("from", "elsewhere"), 0.0, "no group, no blend");
    }
}
