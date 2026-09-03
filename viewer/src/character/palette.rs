//! The colours character creation offers, out of `chara/xls/charaMake/human.cmp`.
//!
//! Every palette is laid out as a grid eight wide, so a swatch's place in the creator is its index
//! over eight. Each carries the colour twice: the first block is what the shader multiplies an
//! albedo by and the second is the swatch the creator shows, which are not the same. Skin makes the
//! difference plain: the multiplier is all but neutral, since the texture already carries the tone,
//! while the swatch is the tone itself.

use anyhow::Result;
use egui::Color32;
use ironworks::file::{File, cmp};

use crate::backend::Backend;

pub const PATH: &str = "chara/xls/charaMake/human.cmp";

/// How wide the creator lays a palette out.
pub const COLUMNS: usize = 8;

/// A palette the creator picks from: what the shader is given, and what the player is shown.
#[derive(Default)]
pub struct Swatches {
    held: Vec<[f32; 4]>,
    shown: Vec<Color32>,
}

impl Swatches {
    /// The swatch at an index, and the colour the shader takes for it.
    pub fn shown(&self, index: usize) -> Option<Color32> {
        self.shown.get(index).copied()
    }

    /// The colour a shader is given, which goes in squared. The fourth lane is not a colour and
    /// passes through as the file states it: a lip and a face paint are worn at the weight their
    /// own swatch carries.
    pub fn shaded(&self, index: usize) -> [f32; 4] {
        let [red, green, blue, weight] = self.held.get(index).copied().unwrap_or([1.0; 4]);
        [red * red, green * green, blue * blue, weight]
    }
}

/// Every palette one clan and gender is offered, plus the ones the whole game shares.
#[derive(Default)]
pub struct Palettes {
    pub skin: Swatches,
    pub hair: Swatches,
    pub highlights: Swatches,
    pub eyes: Swatches,
    pub lips: Swatches,
    pub features: Swatches,
    pub face_paint: Swatches,
    /// How tall a body may be built, as the file states the clan's range.
    pub height: [f32; 2],
    /// What a bust may be scaled to, three axes in the bone's own frame.
    pub bust: [[f32; 3]; 2],
}

/// The file, read once and asked for a clan at a time.
pub struct Made(cmp::CharacterMakeParameters);

impl Made {
    pub async fn read(backend: &Backend) -> Result<Self> {
        let bytes = backend.files().read(PATH).await?;
        Ok(Self(cmp::CharacterMakeParameters::read(
            std::io::Cursor::new(bytes),
        )?))
    }

    /// What one clan and gender may be built from. The block a clan's colours sit in is its row in
    /// `Tribe` counted from nought, doubled, and one past that for a woman.
    pub fn palettes(&self, tribe: u32, female: bool) -> Palettes {
        let at = (tribe.max(1) as usize - 1) * 2 + usize::from(female);
        let (shared, shown) = (self.0.colors(), self.0.interface_colors());
        let Some(clan) = self.0.races().get(at) else {
            return Palettes::default();
        };
        let scale = self.0.scales()[(tribe.max(1) as usize - 1) / 2][(tribe.max(1) as usize - 1) % 2];
        Palettes {
            skin: pair(clan.skin(), clan.skin_interface()),
            hair: pair(
                &clan.hair().map(|held| held.main()),
                clan.hair_interface(),
            ),
            highlights: pair(shared.hair_highlights(), shown.hair_highlights()),
            eyes: pair(shared.eyes(), shown.eyes()),
            lips: pair(shared.lips(), shown.lips()),
            features: pair(shared.features(), shown.features()),
            face_paint: pair(shared.face_paint(), shown.face_paint()),
            height: match female {
                true => [scale.female_min_height(), scale.female_max_height()],
                false => [scale.male_min_height(), scale.male_max_height()],
            },
            bust: [scale.bust_min(), scale.bust_max()],
        }
    }
}

fn pair(held: &[cmp::Color], shown: &[cmp::Color]) -> Swatches {
    Swatches {
        held: held.iter().map(lanes).collect(),
        shown: shown.iter().map(color).collect(),
    }
}

/// A swatch as the file holds it, each channel over the range it is stated in.
fn lanes(held: &cmp::Color) -> [f32; 4] {
    let over = |channel: u8| f32::from(channel) / 255.0;
    [
        over(held.red()),
        over(held.green()),
        over(held.blue()),
        over(held.alpha()),
    ]
}

/// A swatch as the creator draws it. Lips and face paint are offered twice over, the same colours
/// at two weights, and the alpha the file gives each is all that tells the two halves apart.
fn color(held: &cmp::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(held.red(), held.green(), held.blue(), held.alpha())
}
