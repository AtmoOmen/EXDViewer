//! Weapons: which model an item is, where it attaches, and which stance it plays.
//!
//! `Item.ModelMain`/`ModelSub` pack a weapon's set, base and variant the same way `Gear` packs
//! equipment, but sixteen bits a field rather than eight: measured against real weapons (id 100
//! "Dated Bronze Gladius" reads `201/1/4`, id 124 "Dated Bone Hora" reads `301/9/2`), the top
//! sixteen bits are unused in every weapon Item sampled. `ModelSub` is the other hand's model for
//! most of them, but a fist weapon's is a hands equipment id: see [`FISTS`]. Where a weapon
//! attaches to the skeleton comes from the character's own `.atch` file, keyed by a three-letter
//! tag this module derives from the item's `ItemUICategory` (the job the weapon belongs to).

use anyhow::Result;
use ironworks::excel::Language;
use ironworks::file::File;
use ironworks::file::atch::AttachPoints;
use std::io::Cursor;

use crate::backend::Backend;
use crate::character::Gear;
use crate::excel::provider::{ExcelProvider, ExcelSheet as _};

/// `Item`'s model quads, name, icon, `ItemUICategory` and `EquipSlotCategory`, as byte offsets.
const MODEL_MAIN: u32 = 24;
const MODEL_SUB: u32 = 32;
const NAME: u32 = 12;
const ICON: u32 = 136;
const UI_CATEGORY: u32 = 152;
const SLOT_CATEGORY: u32 = 154;
/// Where `EquipSlotCategory` states whether a row fills the main hand or the off hand.
const MAIN_HAND: u32 = 0;
const OFF_HAND: u32 = 1;

/// The sets a fist weapon is filed under. `DrawDataContainer::LoadWeapon` reads the off-hand model
/// of a main hand in this range as the main's own plus fifty rather than off the item, and
/// `LoadEquipment` then draws the hands from what the item's `ModelSub` names.
const FISTS: std::ops::RangeInclusive<u16> = 1601..=1650;

/// A weapon model: the set its directory is filed under, the body within it, and the material
/// colourway. Packed the same shape as [`super::Gear`] but sixteen bits a field rather than eight.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Weapon {
    pub set: u16,
    pub base: u16,
    pub variant: u16,
}

impl Weapon {
    pub fn read(quad: u64) -> Option<Self> {
        (quad != 0).then_some(Self {
            set: quad as u16,
            base: (quad >> 16) as u16,
            variant: (quad >> 32) as u16,
        })
    }

    pub fn model(&self) -> String {
        format!(
            "chara/weapon/w{0:04}/obj/body/b{1:04}/model/w{0:04}b{1:04}.mdl",
            self.set, self.base
        )
    }
}

/// One weapon the game names, and what wearing it takes.
#[derive(Clone)]
pub struct Piece {
    pub name: String,
    pub icon: u32,
    pub weapon: Weapon,
    /// The other hand's own model, where this item carries one of its own: a fist weapon or a
    /// twinblade is one item with two, rather than a second item worn in the other slot.
    pub off_hand: Option<Weapon>,
    /// The gauntlets a fist weapon is worn with, which the game draws in the hands slot over
    /// whatever is worn there.
    pub gauntlets: Option<Gear>,
    /// Whether this item's own `EquipSlotCategory` covers the off hand, leaving nothing there to
    /// pick by hand.
    pub covers_off_hand: bool,
    /// The `.atch` point this weapon hangs from, out of its `ItemUICategory`.
    pub tag: Option<&'static str>,
}

/// The `ItemUICategory` row a combat job's weapon is filed under, and the `.atch` tag it hangs
/// from. Lifted from Penumbra's `AtchType`, which already names these tags by job; a few beyond
/// its own list (`clw`, `pic`) are read off the tag's own spelling rather than a named source.
const TAGS: &[(u16, &str)] = &[
    (1, "fsw"),   // Pugilist's Arm
    (2, "swd"),   // Gladiator's Arm
    (3, "2ax"),   // Marauder's Arm
    (4, "2bw"),   // Archer's Arm
    (5, "2sp"),   // Lancer's Arm
    (6, "rod"),   // One-handed Thaumaturge's Arm
    (7, "2st"),   // Two-handed Thaumaturge's Arm
    (8, "stf"),   // One-handed Conjurer's Arm
    (9, "2st"),   // Two-handed Conjurer's Arm
    (10, "2bk"),  // Arcanist's Grimoire
    (11, "sld"),  // Shield
    (84, "dgr"),  // Rogue's Arm
    (87, "2sw"),  // Dark Knight's Arm
    (88, "2gn"),  // Machinist's Arm
    (89, "2gl"),  // Astrologian's Arm
    (96, "2kt"),  // Samurai's Arm
    (97, "2rp"),  // Red Mage's Arm
    (98, "2bk"),  // Scholar's Arm
    (105, "stf"), // Blue Mage's Arm, unconfirmed
    (106, "2gb"), // Gunbreaker's Arm
    (107, "chk"), // Dancer's Arm
    (108, "2km"), // Reaper's Arm
    (109, "2ff"), // Sage's Arm
    (110, "clw"), // Viper's Arm
    (111, "pic"), // Pictomancer's Arm
];

fn tag(category: u16) -> Option<&'static str> {
    TAGS.iter()
        .find(|(id, _)| *id == category)
        .map(|(_, tag)| *tag)
}

/// Every weapon and shield the game names, split by which hand it is picked for, in one list per
/// hand.
pub type Pieces = (Vec<Piece>, Vec<Piece>);

/// Every weapon and shield the game names, split by which hand it is picked for. A category that
/// covers the off hand rather than filling it (a fist weapon's second knuckle) never lists
/// anything for that hand: the item's own `off_hand` supplies it instead.
pub async fn read(backend: &Backend, language: Language) -> Result<Pieces> {
    let excel = backend.excel();
    let items = excel.get_sheet("Item", language).await?;
    let categories = excel.get_sheet("EquipSlotCategory", language).await?;

    let mut hands = std::collections::BTreeMap::new();
    for id in categories.get_row_ids() {
        let Ok(row) = categories.get_row(id) else {
            continue;
        };
        let main = row.read::<i8>(MAIN_HAND).ok() == Some(1);
        let off = row.read::<i8>(OFF_HAND).ok();
        if main || off == Some(1) || off == Some(-1) {
            hands.insert(id, (main, off == Some(1), off == Some(-1)));
        }
    }

    let (mut main_hand, mut off_hand) = (Vec::new(), Vec::new());
    for id in items.get_row_ids() {
        let Ok(row) = items.get_row(id) else {
            continue;
        };
        let Some(&(fills_main, fills_off, covers_off)) = row
            .read::<u8>(SLOT_CATEGORY)
            .ok()
            .and_then(|category| hands.get(&u32::from(category)))
        else {
            continue;
        };
        let Some(weapon) = row.read::<u64>(MODEL_MAIN).ok().and_then(Weapon::read) else {
            continue;
        };
        let Ok(name) = row.read_string(NAME) else {
            continue;
        };
        let name = name.to_string();
        if name.is_empty() {
            continue;
        }
        // A fist weapon's `ModelSub` is a hands equipment id rather than a weapon, and the knuckle
        // in the other hand is the main's own set plus fifty.
        let sub = row.read::<u64>(MODEL_SUB).ok().unwrap_or(0);
        let fists = FISTS.contains(&weapon.set);
        let piece = Piece {
            name,
            icon: row.read::<u16>(ICON).unwrap_or(0).into(),
            weapon,
            off_hand: match fists {
                true => Some(Weapon { set: weapon.set + 50, ..weapon }),
                false => Weapon::read(sub),
            },
            gauntlets: fists.then(|| Gear::read(sub)).flatten(),
            covers_off_hand: covers_off,
            tag: row
                .read::<u8>(UI_CATEGORY)
                .ok()
                .and_then(|category| tag(u16::from(category))),
        };
        if fills_main {
            main_hand.push(piece.clone());
        }
        if fills_off {
            off_hand.push(piece);
        }
    }
    main_hand.sort_by(|left, right| left.name.cmp(&right.name));
    off_hand.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!(
        "character: {} main hand, {} off hand weapons",
        main_hand.len(),
        off_hand.len()
    );
    Ok((main_hand, off_hand))
}

/// Where a race's `.atch` file is filed, which names its weapon and tool attach points.
pub fn atch_path(code: u16) -> String {
    format!("chara/xls/attachoffset/c{code:04}.atch")
}

/// One placement a weapon's attach point takes.
pub struct Attach {
    pub bone: String,
    pub scale: f32,
    pub offset: [f32; 3],
    pub rotation: [f32; 3],
}

/// The placement `tag` takes in the drawn or the sheathed state, out of a race's own `.atch` file.
/// State 0 is drawn and state 1 is sheathed: measured over every point c0101.atch carries, state 0
/// is the bare, unoffset bone in 106 of 143 and state 1 is a placement of its own in 96 of 143, and
/// no point ever states only the first.
pub fn attach(bytes: &[u8], tag: &str, drawn: bool) -> Option<Attach> {
    let file = AttachPoints::read(Cursor::new(bytes.to_vec())).ok()?;
    let point = file.point(tag)?;
    let state = file.states(point)?.get(usize::from(!drawn))?;
    Some(Attach {
        bone: state.bone().to_owned(),
        scale: state.scale(),
        offset: state.offset(),
        rotation: state.rotation(),
    })
}

/// The bone a weapon hangs from when nothing names an attach point for it: the plain right or left
/// hand null bone, whichever `main` says.
pub fn fallback_bone(main: bool) -> &'static str {
    match main {
        true => "n_buki_r",
        false => "n_buki_l",
    }
}

/// The idle pack a race plays drawn or sheathed. Sheathed is `a0001`, which is what the body plays
/// regardless of what it holds; drawn is `a0034`, a battle idle measured to exist for c0101
/// alongside it (`cbbm_id0`, the `b` where `a0001`'s own idle is `cbnm_id0`'s `n`). Nothing on disk
/// states which of the many other numbered battle idles a given weapon style should play instead,
/// so every drawn weapon shares this one until that is resolved.
pub fn stance_pack(code: u16, drawn: bool) -> String {
    let set = if drawn { 34 } else { 1 };
    format!("chara/human/c{code:04}/animation/a{set:04}/bt_common/resident/idle.pap")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_weapon_quad_packs_set_base_and_variant_in_sixteen_bits_each() {
        assert!(Weapon::read(0).is_none());
        let gladius = Weapon::read(0x0000_0004_0001_00c9).unwrap();
        assert_eq!((gladius.set, gladius.base, gladius.variant), (201, 1, 4));
        assert_eq!(
            gladius.model(),
            "chara/weapon/w0201/obj/body/b0001/model/w0201b0001.mdl"
        );

        let hora = Weapon::read(0x0000_0002_0009_012d).unwrap();
        assert_eq!((hora.set, hora.base, hora.variant), (301, 9, 2));
    }

    #[test]
    fn a_fist_weapon_wears_the_set_past_its_own_and_gauntlets() {
        // "Ultimate Omega Knuckles": set 1601, and a `ModelSub` naming equipment set 8808.
        let knuckles = Weapon::read(0x0000_0002_0002_0641).unwrap();
        assert!(FISTS.contains(&knuckles.set));
        assert_eq!(
            Weapon { set: knuckles.set + 50, ..knuckles }.model(),
            "chara/weapon/w1651/obj/body/b0002/model/w1651b0002.mdl"
        );
        let gauntlets = Gear::read(0x0000_0000_0002_2268).unwrap();
        assert_eq!((gauntlets.set, gauntlets.variant), (8808, 2));

        // "Dated Bone Hora", whose own `ModelSub` is the second knuckle rather than gauntlets.
        assert!(!FISTS.contains(&301));
    }

    #[test]
    fn known_jobs_resolve_a_tag() {
        assert_eq!(tag(2), Some("swd"));
        assert_eq!(tag(96), Some("2kt"));
        assert_eq!(tag(63), None);
    }
}
