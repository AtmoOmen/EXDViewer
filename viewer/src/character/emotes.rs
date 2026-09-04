//! The emotes the game names, out of `Emote` and `ActionTimeline`.
//!
//! An emote names seven timelines by slot: the pose it stands in, the motion that plays it in, and
//! then the same emote sat on the ground, sat on a chair, mounted and asleep. A timeline's key is
//! the tail of a pack path under the body's own animation directory, which is what turns thousands
//! of numbered files into a named, iconned list.
//!
//! The game does not mask an emote down to the upper body: it plays a different motion. The three
//! seated slots name a `u_` pack of their own holding one `cbep_u_` motion, which is a partial
//! naming only the bones it moves, so whatever the body is held in shows through the rest. 232 of
//! the 300 emotes the sheet names state one for at least one of those slots, and every one of them
//! is filed under `Stance` 1 where the standing motion is filed under 0.

use anyhow::Result;
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::{ExcelProvider, ExcelRow, ExcelSheet};

/// `Emote`'s name, icon and the two timelines a standing character plays, and `ActionTimeline`'s
/// key, as byte offsets.
const NAME: u32 = 0;
const ICON: u32 = 4;
const STANDING: u32 = 16;
const START: u32 = 18;
/// The slot an emote played while mounted reads, which is the third of the four seated ones.
const MOUNTED: u32 = 24;
const KEY: u32 = 0;

/// One emote the creator's own list would show.
pub struct Emote {
    pub name: String,
    pub icon: u32,
    /// The pose it holds and the motion that plays it in, as the keys they are filed under. An
    /// emote that only makes a face states one and no other.
    standing: Option<String>,
    start: Option<String>,
    /// The partial the same emote is played as while mounted, where it names one.
    mounted: Option<String>,
}

impl Emote {
    /// The keys the packs a body plays this from are filed under: the motion it starts with, and
    /// the pose it settles into once that has played through. An emote that holds a pose forever
    /// states the motion that plays it in apart from the pose itself; one that only moves states
    /// the motion alone.
    pub fn keys(&self) -> (Option<&str>, Option<&str>) {
        match (&self.start, &self.standing) {
            (Some(start), standing) => (Some(start), standing.as_deref()),
            (None, standing) => (standing.as_deref(), None),
        }
    }

    /// The key of the partial this emote is played as while mounted, which is laid over the pose
    /// the mount holds the rider in rather than replacing it.
    pub fn mounted(&self) -> Option<&str> {
        self.mounted.as_deref()
    }

    /// The expression this emote is, for the ones that only make a face. Those are filed under the
    /// face skeleton a character wears rather than under its body, and the last segment of the key
    /// is what names one there.
    pub fn expression(&self) -> Option<&str> {
        let key = self.standing.as_deref()?.strip_prefix("facial/")?;
        key.rsplit('/').next()
    }
}

/// Every emote the game both names and animates, in name order.
pub async fn read(backend: &Backend, language: Language) -> Result<Vec<Emote>> {
    let excel = backend.excel();
    let emotes = excel.get_sheet("Emote", language).await?;
    let timelines = excel.get_sheet("ActionTimeline", language).await?;

    let mut found = Vec::new();
    for id in emotes.get_row_ids() {
        let Ok(row) = emotes.get_row(id) else {
            continue;
        };
        let (Ok(name), Ok(icon)) = (row.read_string(NAME), row.read::<u32>(ICON)) else {
            continue;
        };
        let name = name.to_string();
        if name.is_empty() || icon == 0 {
            continue;
        }
        let slot = |at| {
            let timeline = row.read::<u16>(at).ok().filter(|timeline| *timeline > 0)?;
            key(&timelines, u32::from(timeline))
        };
        let (standing, start) = (slot(STANDING), slot(START));
        if standing.is_none() && start.is_none() {
            continue;
        }
        found.push(Emote {
            name,
            icon,
            standing,
            start,
            mounted: slot(MOUNTED),
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!("character: {} emotes to play", found.len());
    Ok(found)
}

fn key(timelines: &impl ExcelSheet, id: u32) -> Option<String> {
    let row: ExcelRow<'_> = timelines.get_row(id).ok()?;
    let key = row.read_string(KEY).ok()?.to_string();
    (!key.is_empty()).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::Emote;

    fn emote(standing: Option<&str>, start: Option<&str>) -> Emote {
        Emote {
            name: String::new(),
            icon: 0,
            standing: standing.map(ToOwned::to_owned),
            start: start.map(ToOwned::to_owned),
            mounted: None,
        }
    }

    /// A pose held forever states the motion that plays it in apart from the pose itself, and the
    /// motion comes first.
    #[test]
    fn an_emote_starts_before_it_settles() {
        let sit = emote(Some("emote/sit"), Some("event_base/event_base_chair_start"));
        assert_eq!(
            sit.keys(),
            (Some("event_base/event_base_chair_start"), Some("emote/sit"))
        );
        assert_eq!(sit.expression(), None);

        let wave = emote(Some("emote/goodbye_st"), None);
        assert_eq!(wave.keys(), (Some("emote/goodbye_st"), None));
    }

    /// An emote that only makes a face is filed under the face a character wears, not under its
    /// body, and the last segment of the key is what names it there.
    #[test]
    fn an_emote_that_only_makes_a_face_names_an_expression() {
        assert_eq!(emote(Some("facial/pose/smile"), None).expression(), Some("smile"));
        assert_eq!(emote(Some("facial/pose/base"), None).expression(), Some("base"));
        assert_eq!(emote(Some("emote/bow"), None).expression(), None);
    }

    /// What the mounted slot really names, off the real install: Blow Bubbles states `u_sp63` for
    /// every seated slot, and that pack holds one `cbep_u_sp63` moving a fraction of the bones the
    /// whole-body `cbem_sp63` does. That is the whole of how the game restricts an emote to the
    /// upper half - a motion of its own, not a mask over the standing one.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn the_mounted_slot_names_a_partial_that_moves_fewer_bones() {
        use ironworks::file::File as _;
        use ironworks::file::pap::AnimationPack;
        use ironworks::sqpack::{Install, SqPack};

        let install = ironworks::Ironworks::new()
            .with_resource(SqPack::new(Install::at_sqpack(
                "/home/asriel/.xlcore/ffxiv/game/sqpack",
            )));
        let moved = |key: &str| -> (String, usize) {
            let path =
                format!("chara/human/c0101/animation/a0001/bt_common/{key}.pap");
            let bytes: Vec<u8> = install.file(&path).expect(&path);
            let pack = AnimationPack::read(std::io::Cursor::new(bytes)).expect("a readable pack");
            let bindings = pack.parse_animations().expect("its motions");
            (
                pack.animations()[0].name().to_owned(),
                bindings[0].bones().len(),
            )
        };

        let (whole, all) = moved("emote_sp/sp63");
        let (partial, some) = moved("emote_sp/u_sp63");
        assert_eq!(whole, "cbem_sp63");
        assert_eq!(partial, "cbep_u_sp63");
        assert!(some < all, "{partial} moves {some} bones, {whole} moves {all}");
    }
}
