//! Private-use codepoints `FFXIV_Lodestone_SSF.ttf` (loaded into the Proportional font family in
//! `app.rs`) draws as the game's own inline badges, matching Dalamud's `SeIconChar` table.

use ironworks::excel::Language;

pub const HIGH_QUALITY: char = '\u{E03C}';
pub const GIL: char = '\u{E049}';

/// The "Lv" badge, localized the way the game ships a client-language variant of it.
pub fn level(language: Language) -> char {
    match language {
        Language::German => '\u{E06B}',
        Language::French => '\u{E06C}',
        _ => '\u{E06A}',
    }
}
