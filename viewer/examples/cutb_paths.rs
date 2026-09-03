//! Dumps the `.cutb` paths the `Cutscene` sheet names.

use ironworks::{
    Ironworks,
    excel::{Excel, Field, Language},
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn main() {
    let ironworks: std::sync::Arc<Ironworks> = std::sync::Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks).with_default_language(Language::English);
    let sheet = excel.sheet("Cutscene").expect("Cutscene");
    let columns = sheet.columns().expect("columns");
    eprintln!("{} columns", columns.len());
    for row in sheet {
        for column in &columns {
            if let Ok(Field::String(text)) = row.field(column) {
                let stem = text.to_string();
                if !stem.is_empty() {
                    println!("cut/{stem}.cutb");
                }
            }
        }
    }
}
