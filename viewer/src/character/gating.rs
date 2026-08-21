//! What a worn piece does to the body under it, out of `chara/xls/equipmentparameter/equipmentparameter.eqp`.
//!
//! A set states, per slot, which of the body's own models still draw beneath it. That is what keeps
//! a character's bare legs out of a full-length coat, and it is the file's answer rather than a
//! depth bias: the two meshes are the same skin where a race's smallclothes are its own body.

use anyhow::Result;
use ironworks::file::{File, eqp};

use super::Slot;
use crate::backend::Backend;

pub const PATH: &str = "chara/xls/equipmentparameter/equipmentparameter.eqp";

/// The seams themselves, each named for the part of the body it sits at. A name belongs to one
/// slot's models alone, so hiding it by name reaches only the model that owns it.
const NECK: &str = "atr_nek";
const UPPER_ARM: &str = "atr_ude";
const FOREARM: &str = "atr_hij";
const WAIST: &str = "atr_kod";
const KNEE: &str = "atr_hiz";
const CALF: &str = "atr_sne";
const KNEE_PAD: &str = "atr_lpd";

/// The file, read once and asked about a set at a time.
pub struct Worn(eqp::EquipmentParameter);

impl Worn {
    pub async fn read(backend: &Backend) -> Result<Self> {
        let bytes = backend.files().read(PATH).await?;
        Ok(Self(eqp::EquipmentParameter::read(std::io::Cursor::new(
            bytes,
        ))?))
    }

    /// Whether the model for one slot still draws under a piece worn in another: the body's own
    /// where the slot is gear, and the adornment itself where it is not, since nothing stands in
    /// for a ring. A set the file leaves disabled says nothing, so both draw as they would bare.
    ///
    /// Which earrings a helmet leaves on is stated per race rather than once, ears being where the
    /// races differ most: a hat that clears a Hyur's ear sits over a Miqo'te's.
    pub fn shows(&self, worn: Slot, set: u16, under: Slot, race: u32) -> bool {
        let held = self.0.set(set);
        match worn {
            Slot::Head => {
                let head = held.head();
                match (head.enabled(), under) {
                    (false, _) => true,
                    (_, Slot::Neck) => head.show_necklace(),
                    (_, Slot::Ears) => match race {
                        2 | 3 => head.show_earrings_elezen_lalafell(),
                        4 | 7 | 8 => head.show_earrings_miqote_hrothgar_viera(),
                        6 => head.show_earrings_au_ra(),
                        _ => head.show_earrings_hyur_roegadyn(),
                    },
                    _ => true,
                }
            }
            Slot::Body => {
                let body = held.body();
                match (body.enabled(), under) {
                    (false, _) => true,
                    (_, Slot::Legs) => body.show_legs(),
                    (_, Slot::Hands) => body.show_hands(),
                    (_, Slot::Head) => body.show_head(),
                    (_, Slot::Neck) => body.show_necklace(),
                    (_, Slot::Wrists) => body.show_bracelets(),
                    _ => true,
                }
            }
            Slot::Hands => {
                let hands = held.hands();
                match (hands.enabled(), under) {
                    (false, _) => true,
                    (_, Slot::Wrists) => hands.show_bracelets(),
                    (_, Slot::RingLeft) => hands.show_ring_left(),
                    (_, Slot::RingRight) => hands.show_ring_right(),
                    _ => true,
                }
            }
            Slot::Legs => {
                let legs = held.legs();
                match (legs.enabled(), under) {
                    (false, _) => true,
                    (_, Slot::Feet) => legs.show_feet(),
                    _ => true,
                }
            }
            _ => true,
        }
    }

    /// The parts a piece worn in one slot covers on the models under it, by the name those models
    /// file them under. Each is a seam: a garment reaches over one of them and the geometry beneath
    /// would poke through, so the file states which to leave undrawn rather than the two being told
    /// apart by depth.
    pub fn covers(&self, worn: Slot, set: u16) -> Vec<&'static str> {
        // Smallclothes have no entry of their own, entry nought being the file's own control word,
        // and reach over nothing: taking the next set's leaves a bare leg with its knee cut out.
        if set == 0 {
            return Vec::new();
        }
        let held = self.0.set(set);
        let mut found = Vec::new();
        match worn {
            Slot::Head => {
                let head = held.head();
                if head.enabled() && head.hide_neck() {
                    found.push(NECK);
                }
            }
            Slot::Body => {
                let body = held.body();
                if body.enabled() && body.hide_waist() {
                    found.push(WAIST);
                }
            }
            Slot::Hands => {
                // The two bits are one reach rather than two seams: a glove ends at the wrist, the
                // forearm, the elbow or the upper arm, and only the last two reach over anything.
                let hands = held.hands();
                if hands.enabled() && hands.hide_forearm() {
                    found.push(FOREARM);
                    if hands.hide_elbow() {
                        found.push(UPPER_ARM);
                    }
                }
            }
            Slot::Legs => {
                let legs = held.legs();
                if legs.enabled() && legs.hide_knee_pads() {
                    found.push(KNEE_PAD);
                }
            }
            Slot::Feet => {
                // A reach again: a boot ends at the ankle, the calf or the knee, and the shoe that
                // ends below the calf covers neither.
                let feet = held.feet();
                if feet.enabled() && feet.hide_calf() {
                    found.push(CALF);
                    if feet.hide_knee() {
                        found.push(KNEE);
                    }
                }
            }
            // An adornment sits over a garment rather than through it, and the file names no seam
            // for one.
            _ => {}
        }
        found
    }

    /// Whether a hat leaves the hair on. A head set that states nothing leaves it drawn.
    pub fn keeps_hair(&self, set: u16) -> bool {
        let head = self.0.set(set).head();
        !head.enabled() || !head.hide_hair() || head.show_hair_override()
    }
}
