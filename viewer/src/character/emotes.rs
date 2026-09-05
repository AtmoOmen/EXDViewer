//! The emotes the game names, out of `Emote` and `ActionTimeline`.
//!
//! An emote names seven timelines by slot: the pose it stands in, the motion that plays it in, and
//! then the same emote sat on the ground, sat on a chair, mounted and asleep, none of which the
//! creator's character ever is. A timeline's key is the tail of a pack path under the body's own
//! animation directory, which is what turns thousands of numbered files into a named, iconned list.

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
const KEY: u32 = 0;

/// One emote the creator's own list would show.
pub struct Emote {
    pub name: String,
    pub icon: u32,
    /// The pose it holds and the motion that plays it in, as the keys they are filed under. An
    /// emote that only makes a face states one and no other.
    standing: Option<String>,
    start: Option<String>,
}

impl Emote {
    /// The packs a body plays this from, the motion it starts with first. A key is filed under the
    /// body's own code, so a character of another race reads the same emote out of its own
    /// directory.
    pub fn packs(&self, code: u16) -> Vec<String> {
        [&self.start, &self.standing]
            .into_iter()
            .flatten()
            .map(|key| format!("chara/human/c{code:04}/animation/a0001/bt_common/{key}.pap"))
            .collect()
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
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    log::info!("角色：可播放 {} 个情感动作", found.len());
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
        }
    }

    /// A pose held forever states the motion that plays it in apart from the pose itself, and the
    /// motion comes first.
    #[test]
    fn an_emote_starts_before_it_settles() {
        let sit = emote(Some("emote/sit"), Some("event_base/event_base_chair_start"));
        assert_eq!(
            sit.packs(101),
            [
                "chara/human/c0101/animation/a0001/bt_common/event_base/event_base_chair_start.pap",
                "chara/human/c0101/animation/a0001/bt_common/emote/sit.pap",
            ]
        );
        assert_eq!(sit.expression(), None);

        let wave = emote(Some("emote/goodbye_st"), None);
        assert_eq!(
            wave.packs(1101),
            ["chara/human/c1101/animation/a0001/bt_common/emote/goodbye_st.pap"]
        );
    }

    /// An emote that only makes a face is filed under the face a character wears, not under its
    /// body, and the last segment of the key is what names it there.
    #[test]
    fn an_emote_that_only_makes_a_face_names_an_expression() {
        assert_eq!(emote(Some("facial/pose/smile"), None).expression(), Some("smile"));
        assert_eq!(emote(Some("facial/pose/base"), None).expression(), Some("base"));
        assert_eq!(emote(Some("emote/bow"), None).expression(), None);
    }
}
