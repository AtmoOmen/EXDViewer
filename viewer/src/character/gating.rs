//! What a worn piece does to the body under it, out of `chara/xls/equipmentparameter/equipmentparameter.eqp`.
//!
//! A set states, per slot, which of the body's own models still draw beneath it. That is what keeps
//! a character's bare legs out of a full-length coat, and it is the file's answer rather than a
//! depth bias: the two meshes are the same skin where a race's smallclothes are its own body.

use std::collections::BTreeSet;

use anyhow::Result;
use ironworks::file::{File, eqp};

use super::{Outfit, Slot};
use crate::backend::Backend;

pub const PATH: &str = "chara/xls/equipmentparameter/equipmentparameter.eqp";

/// The seams themselves, each named for the part of the body it sits at. Nearly every name
/// belongs to one slot's models, so hiding it by name reaches only the model that owns it.
const NECK: &str = "atr_nek";
const UPPER_ARM: &str = "atr_ude";
const FOREARM: &str = "atr_hij";
const WAIST: &str = "atr_kod";
const KNEE: &str = "atr_hiz";
const CALF: &str = "atr_sne";
const KNEE_PAD: &str = "atr_lpd";
const CUFF: &str = "atr_arm";
const SHAFT: &str = "atr_leg";
const GORGET: &str = "atr_inr";

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

    /// The parts the outfit covers on the models under it, by the name those models file them
    /// under. Each is a seam: two pieces reach over the same stretch of body and the geometry of
    /// one would poke through the other, so the file states which to leave undrawn rather than the
    /// two being told apart by depth.
    ///
    /// A sleeve and a cuff, or a hem and a boot shaft, both claim the same stretch. Which of them
    /// gives up its own seam is the reach each states rather than either one always winning, so
    /// the pair is read together and a piece still on its way states nothing.
    pub fn covers(&self, outfit: &Outfit) -> BTreeSet<&'static str> {
        // Smallclothes have no entry of their own, entry nought being the file's own control word,
        // and reach over nothing: taking the next set's leaves a bare leg with its knee cut out.
        let stated = |slot: Slot| {
            outfit[slot as usize]
                .map(|gear| gear.set)
                .filter(|set| *set != 0)
                .map(|set| self.0.set(set))
        };
        let body = stated(Slot::Body);
        let legs = stated(Slot::Legs);
        let hands = stated(Slot::Hands);
        let feet = stated(Slot::Feet);
        let head = stated(Slot::Head);
        let sleeve = body.as_ref().map_or(0, |held| held.body().sleeve_reach());
        let cuff = hands.as_ref().map_or(0, |held| held.hands().cuff_reach());
        let hem = legs.as_ref().map_or(0, |held| held.legs().hem_reach());
        let shaft = feet.as_ref().map_or(0, |held| held.feet().shaft_reach());

        let mut found = BTreeSet::new();
        if let Some(body) = body.as_ref().map(eqp::Set::body).filter(eqp::Body::enabled) {
            // Both bits sit at the waistband, one for a garment that reaches the waist and one for
            // a coat that goes on past the knee and takes the pad with it.
            if body.hide_waist() || body.hide_thighs() {
                found.insert(WAIST);
            }
            if body.hide_thighs() && !body.hide_waist() {
                found.insert(KNEE_PAD);
            }
            if body.hide_gorget() {
                found.insert(GORGET);
            }
            if sleeve > cuff {
                found.insert(CUFF);
            }
        }
        if let Some(hands) = hands.as_ref().map(eqp::Set::hands).filter(eqp::Hands::enabled)
            && sleeve <= cuff
            && hands.hide_forearm()
        {
            // The two bits are one reach rather than two seams: a glove ends at the wrist, the
            // forearm, the elbow or the upper arm, and only the last two reach over anything.
            found.insert(FOREARM);
            if hands.hide_elbow() {
                found.insert(UPPER_ARM);
            }
        }
        if let Some(legs) = legs.as_ref().map(eqp::Set::legs).filter(eqp::Legs::enabled) {
            if legs.hide_knee_pads() {
                found.insert(KNEE_PAD);
            }
            if hem > shaft {
                found.insert(SHAFT);
                found.insert(KNEE_PAD);
            }
        }
        if let Some(feet) = feet.as_ref().map(eqp::Set::feet).filter(eqp::Feet::enabled)
            && hem <= shaft
            && feet.hide_calf()
        {
            // A reach again: a boot ends at the ankle, the calf or the knee, and the shoe that
            // ends below the calf covers neither.
            found.insert(CALF);
            if feet.hide_knee() {
                found.insert(KNEE);
            }
        }
        // A helmet takes the collar off what is under it, unless the piece there states a gorget
        // of its own, which is the helmet's own neck piece rather than the collar.
        if let Some(head) = head.as_ref().map(eqp::Set::head).filter(eqp::Head::enabled)
            && head.hide_neck()
            && !body.as_ref().is_some_and(|held| held.body().hide_gorget())
        {
            found.insert(NECK);
        }
        found
    }

    /// Whether a hat leaves the hair on. A head set that states nothing leaves it drawn.
    pub fn keeps_hair(&self, set: u16) -> bool {
        let head = self.0.set(set).head();
        !head.enabled() || !head.hide_hair() || head.show_hair_override()
    }
}
