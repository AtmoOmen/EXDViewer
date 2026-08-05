/// Quest row ids start here, and the generated per-quest files bucket by hundreds of rows.
const FIRST_ROW: u32 = 65536;

fn folder(row_id: u32) -> u32 {
    row_id.saturating_sub(FIRST_ROW) / 100
}

/// The quest's own dialogue sheet, which ships no schema and reads as two raw string columns.
pub fn text_sheet(row_id: u32, id: &str) -> String {
    format!("quest/{:03}/{id}", folder(row_id))
}

pub fn script_path(row_id: u32, id: &str) -> String {
    format!("game_script/quest/{:03}/{id}.luab", folder(row_id))
}

pub fn cutscene_path(stem: &str) -> String {
    format!("cut/{stem}.cutb")
}

/// Where a dialogue row belongs, read off its row key.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    Journal,
    Objective,
    System,
    Speaker(&'a str),
}

/// Keys read `TEXT_{ID}_{BUCKET}_{...}`, where a bucket the game does not reserve is a speaker.
pub fn line_of<'a>(key: &'a str, id_upper: &str) -> Line<'a> {
    let rest = key
        .strip_prefix("TEXT_")
        .and_then(|key| key.strip_prefix(id_upper))
        .and_then(|key| key.strip_prefix('_'))
        .unwrap_or(key);
    match rest.split('_').next().unwrap_or(rest) {
        "SEQ" => Line::Journal,
        "TODO" => Line::Objective,
        "SYSTEM" => Line::System,
        speaker => Line::Speaker(speaker),
    }
}

/// The sheet a `QuestParams` entry's `ScriptArg` indexes, for the instruction names that were
/// measured to always name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Bgm,
    Cutscene,
}

/// `LCUT_ACTION`, `LCUT_MOTION` and `LCUT_ACTOR*` carry animation and actor ids, so anything
/// looser than these four prefixes reads unrelated namespaces as cutscene rows.
pub fn param_of(instruction: &str) -> Option<Param> {
    const CUTSCENE: [&str; 4] = ["CUTSCENE", "CUT_SCENE", "CUT_EVENT", "NCUT_"];
    if instruction.contains("BGM") {
        Some(Param::Bgm)
    } else if CUTSCENE.iter().any(|name| instruction.starts_with(name)) {
        Some(Param::Cutscene)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_quest_files_bucket_by_hundreds() {
        assert_eq!(
            text_sheet(65575, "ManFst001_00039"),
            "quest/000/ManFst001_00039"
        );
        assert_eq!(
            script_path(70000, "AktKmg115_04464"),
            "game_script/quest/044/AktKmg115_04464.luab"
        );
    }

    #[test]
    fn a_row_key_names_its_bucket_or_its_speaker() {
        let id = "MANFST001_00039";
        assert_eq!(line_of("TEXT_MANFST001_00039_SEQ_02", id), Line::Journal);
        assert_eq!(line_of("TEXT_MANFST001_00039_TODO_00", id), Line::Objective);
        assert_eq!(line_of("TEXT_MANFST001_00039_SYSTEM_000", id), Line::System);
        assert_eq!(
            line_of("TEXT_MANFST001_00039_MIOUNNE_000_22", id),
            Line::Speaker("MIOUNNE")
        );
    }

    #[test]
    fn only_the_measured_instruction_names_point_at_a_sheet() {
        assert_eq!(param_of("LOC_BGM01"), Some(Param::Bgm));
        assert_eq!(param_of("CUT_SCENE_N_01"), Some(Param::Cutscene));
        assert_eq!(param_of("NCUT_00"), Some(Param::Cutscene));
        assert_eq!(param_of("LCUT_ACTION"), None);
        assert_eq!(param_of("LCUT_ACTOR0"), None);
        assert_eq!(param_of("ACTOR0"), None);
    }
}
