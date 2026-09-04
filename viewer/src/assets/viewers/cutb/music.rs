//! What a cutscene plays under. Nothing in a `.cutb` states music: the quest that names the
//! cutscene names its `BGM` rows in the same `QuestParams`, and its script is what plays them.
//!
//! So the link is quest-wide rather than per-cutscene, and every track a quest names is offered.
//! Where the quest's script plays one in the same scene as the cutscene, that one is marked and
//! taken first.

use anyhow::{Result, anyhow};
use ironworks::excel::Language;

use crate::backend::Backend;
use crate::excel::provider::ExcelSheet;
use crate::quests::index::{Fields, integer, text};
use crate::quests::script::{self, Step};
use crate::quests::derive;

/// Slots a quest spends on script parameters.
const PARAMS: usize = 50;

/// One `BGM` row a cutscene's own quest names.
pub struct Track {
    /// The `ScriptInstruction` the quest names the row with, which is what its script calls.
    pub instruction: String,
    pub path: String,
    pub quest: u32,
    /// Whether the quest's script plays it in the scene that plays this cutscene.
    pub scripted: bool,
}

pub struct Music {
    pub tracks: Vec<Track>,
    /// The quests naming the cutscene, whether or not any of them names music.
    pub quests: usize,
}

/// Every `BGM` the quests naming `path` state, with the ones their scripts pair with this
/// cutscene first.
pub async fn resolve(backend: Backend, language: Language, path: String) -> Result<Music> {
    let cutscene = Fields::load(&backend, "Cutscene", Language::None).await?;
    let stem = cutscene.at("Path")?;
    let row_id = cutscene
        .sheet
        .get_row_ids()
        .into_iter()
        .find(|row_id| {
            cutscene
                .sheet
                .get_row(*row_id)
                .is_ok_and(|row| derive::cutscene_path(&text(row, stem)) == path)
        })
        .ok_or_else(|| anyhow!("no Cutscene row names {path}"))?;

    let quests = Fields::load(&backend, "Quest", language).await?;
    let id = quests.at("Id")?;
    let slots: Vec<_> = (0..PARAMS)
        .filter_map(|slot| {
            Some((
                quests
                    .at(&format!("QuestParams[{slot}].ScriptInstruction"))
                    .ok()?
                    .clone(),
                quests.at(&format!("QuestParams[{slot}].ScriptArg")).ok()?.clone(),
            ))
        })
        .collect();

    // Each quest naming the cutscene, the instruction it names it with, and the music it states.
    let mut naming = Vec::new();
    for quest in quests.sheet.get_row_ids() {
        let Ok(row) = quests.sheet.get_row(quest) else {
            continue;
        };
        let mut names = None;
        let mut music = Vec::new();
        for (instruction, arg) in &slots {
            let instruction = text(row, instruction);
            let arg = integer(row, arg);
            match derive::param_of(&instruction) {
                Some(derive::Param::Bgm) => music.push((instruction, arg)),
                Some(derive::Param::Cutscene) if arg == row_id => names = Some(instruction),
                _ => {}
            }
        }
        if let Some(names) = names {
            naming.push((quest, text(row, id), names, music));
        }
    }

    let bgm = Fields::load(&backend, "BGM", Language::None).await?;
    let file = bgm.at("File")?;
    let mut tracks = Vec::new();
    for (quest, quest_id, names, music) in &naming {
        let played = scripted(&backend, *quest, quest_id, names).await;
        for (instruction, arg) in music {
            let Ok(row) = bgm.sheet.get_row(*arg) else {
                continue;
            };
            let path = text(row, file);
            if path.is_empty() {
                continue;
            }
            tracks.push(Track {
                instruction: instruction.clone(),
                path,
                quest: *quest,
                scripted: played.iter().any(|name| name == instruction),
            });
        }
    }
    tracks.sort_by(|left, right| {
        right
            .scripted
            .cmp(&left.scripted)
            .then_with(|| left.instruction.cmp(&right.instruction))
    });
    tracks.dedup_by(|left, right| left.path == right.path);
    Ok(Music {
        tracks,
        quests: naming.len(),
    })
}

/// The `PlayBGM` names a quest's script calls in the scenes that also play `names`. A script that
/// stops the music calls a constant of its own rather than one of the quest's parameters, so a
/// name no parameter carries falls out when the caller matches these against its own list.
async fn scripted(backend: &Backend, quest: u32, quest_id: &str, names: &str) -> Vec<String> {
    let path = derive::script_path(quest, quest_id);
    let Ok(bytes) = backend.files().read(&path).await else {
        return Vec::new();
    };
    let Ok(read) = script::read(&bytes) else {
        return Vec::new();
    };
    let mut played = Vec::new();
    for scene in &read.scenes {
        let music: Vec<&String> = scene
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Bgm(name) => Some(name),
                _ => None,
            })
            .collect();
        let plays = scene
            .steps
            .iter()
            .any(|step| matches!(step, Step::Cutscene(name) if name == names));
        if plays {
            played.extend(music.into_iter().cloned());
        }
    }
    played
}
