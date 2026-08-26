//! Dumps a sheet's columns sorted by offset, to map an EXDSchema field name to the byte offset
//! the viewer's raw readers need. `stain_cols <sheet> [row]`

use std::sync::Arc;

use ironworks::excel::{Excel, Language};
use ironworks::sqpack::{Install, SqPack};
use ironworks::Ironworks;

const SQPACK: &str = "/tmp/xiv/global/latest/game/sqpack";

fn main() {
    let mut args = std::env::args().skip(1);
    let sheet_name = args.next().unwrap_or_else(|| "Stain".to_string());
    let row_id: Option<u32> = args.next().and_then(|s| s.parse().ok());

    let ironworks: Arc<Ironworks> = Arc::new(
        Ironworks::new().with_resource(Box::new(SqPack::new(Install::at_sqpack(SQPACK)))),
    );
    let excel = Excel::new(ironworks).with_default_language(Language::English);
    let sheet = excel.sheet(sheet_name.as_str()).expect("sheet");
    let mut cols = sheet.columns().expect("columns");
    cols.sort_by_key(|c| c.offset());
    for (i, c) in cols.iter().enumerate() {
        println!("{i}: offset={} kind={:?}", c.offset(), c.kind());
    }

    for id in row_id.map(|id| vec![id]).unwrap_or_else(|| vec![1, 4, 7]) {
        let row = sheet.row(id).expect("row");
        println!("-- row {id} --");
        for (i, c) in cols.iter().enumerate() {
            let field = row.field(c);
            println!("{i}: offset={} kind={:?} value={field:?}", c.offset(), c.kind());
        }
    }
}
