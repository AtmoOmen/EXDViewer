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

/// The file, read once and asked about a set at a time.
pub struct Worn(eqp::EquipmentParameter);

impl Worn {
    pub async fn read(backend: &Backend) -> Result<Self> {
        let bytes = backend.files().read(PATH).await?;
        Ok(Self(eqp::EquipmentParameter::read(std::io::Cursor::new(
            bytes,
        ))?))
    }

    /// Whether the body's own model for one slot still draws under a piece worn in another. A set
    /// the file leaves disabled says nothing, so the body draws as it would bare.
    pub fn shows(&self, worn: Slot, set: u16, under: Slot) -> bool {
        let held = self.0.set(set);
        match worn {
            Slot::Body => {
                let body = held.body();
                match (body.enabled(), under) {
                    (false, _) => true,
                    (_, Slot::Legs) => body.show_legs(),
                    (_, Slot::Hands) => body.show_hands(),
                    (_, Slot::Head) => body.show_head(),
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

    /// Whether a hat leaves the hair on. A head set that states nothing leaves it drawn.
    pub fn keeps_hair(&self, set: u16) -> bool {
        let head = self.0.set(set).head();
        !head.enabled() || !head.hide_hair() || head.show_hair_override()
    }
}
