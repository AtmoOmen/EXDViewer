use compact_str::CompactString;
use serde_json::json;

use crate::sheet::CellValue;

use super::{
    handler::{QueryRowsParams, SearchCellsParams},
    process_validate_filter,
};

#[test]
fn local_session_manager_can_disable_keep_alive_timeout() {
    let mut session_manager =
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default();

    assert!(session_manager.session_config.keep_alive.is_some());

    session_manager.session_config.keep_alive = None;

    assert!(session_manager.session_config.keep_alive.is_none());
}

#[test]
fn search_cells_params_reject_filter_field() {
    let err = serde_json::from_value::<SearchCellsParams>(json!({
        "name": "Item",
        "query": "Potion",
        "filter": "Name *= Potion"
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn query_rows_params_reject_query_field() {
    let err = serde_json::from_value::<QueryRowsParams>(json!({
        "name": "Item",
        "query": "Potion"
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn validate_filter_accepts_core_dsl() {
    let result = process_validate_filter(r#"# = 42 OR Name *= "Potion""#);
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(value["valid"], true);
}

#[test]
fn validate_filter_rejects_invalid_dsl() {
    let result = process_validate_filter("((");
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(value["valid"], false);
}

#[test]
fn structured_string_value_includes_display_and_raw_text() {
    let value = CellValue::String(CompactString::from("Hello").into()).to_structured_value();

    assert_eq!(value["kind"], "String");
    assert_eq!(value["display"], "Hello");
    assert_eq!(value["raw"]["macro"], "Hello");
    assert!(value["raw"]["bytes_base64"].is_string());
}

#[test]
fn structured_model_id_value_exposes_parts() {
    let value = CellValue::ModelId(either::Either::Left(0x01020304)).to_structured_value();

    assert_eq!(value["kind"], "ModelId");
    assert_eq!(value["parts"]["model"], 0x0304);
    assert_eq!(value["parts"]["variant"], 0x02);
    assert_eq!(value["parts"]["stain"], 0x01);
}

#[test]
fn structured_link_value_embeds_nested_value() {
    let value = CellValue::ValidLink {
        sheet_name: CompactString::from("Item"),
        row_id: 42,
        value: Some(Box::new(CellValue::Boolean(true))),
    }
    .to_structured_value();

    assert_eq!(value["kind"], "Link");
    assert_eq!(value["state"], "valid");
    assert_eq!(value["display"], "true");
    assert_eq!(value["sheet_name"], "Item");
    assert_eq!(value["row_id"], 42);
    assert_eq!(value["value"]["kind"], "Boolean");
}

/// A `.tmb` of one actor holding one track holding one command, with the id lists in a pool past
/// the items — the same shape the viewer's own tmb tests build.
fn timeline() -> Vec<u8> {
    let (actors, tracks, commands) = (116u32, 118u32, 120u32);
    let mut bytes = Vec::new();
    bytes.extend(b"TMLB");
    bytes.extend(122u32.to_le_bytes());
    bytes.extend(5u32.to_le_bytes());
    bytes.extend(b"TMDH");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(0i16.to_le_bytes());
    bytes.extend(0i16.to_le_bytes());
    bytes.extend(100i16.to_le_bytes());
    bytes.extend(3i16.to_le_bytes());
    bytes.extend(b"TMAL");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend((actors - 36).to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(b"TMAC");
    bytes.extend(28u32.to_le_bytes());
    bytes.extend(1i16.to_le_bytes());
    bytes.extend(0i16.to_le_bytes());
    bytes.extend(0i32.to_le_bytes());
    bytes.extend(0i32.to_le_bytes());
    bytes.extend((tracks - 52).to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(b"TMTR");
    bytes.extend(24u32.to_le_bytes());
    bytes.extend(2i16.to_le_bytes());
    bytes.extend(0i16.to_le_bytes());
    bytes.extend((commands - 80).to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(0i32.to_le_bytes());
    bytes.extend(b"C011");
    bytes.extend(20u32.to_le_bytes());
    bytes.extend(3i16.to_le_bytes());
    bytes.extend(5i16.to_le_bytes());
    bytes.extend(1i32.to_le_bytes());
    bytes.extend(0i32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes
}

#[test]
fn inspect_structures_a_tmb_timeline() {
    use super::assets::inspect;

    let value: serde_json::Value =
        serde_json::from_str(&inspect("cut_example.tmb", &timeline(), 100).unwrap()).unwrap();

    assert_eq!(value["format"]["viewer"], "时间轴");
    assert_eq!(value["details"]["items"], 5);
    assert_eq!(value["details"]["duration"], 100);
    let kinds = value["details"]["kinds"].as_array().unwrap();
    assert_eq!(kinds.len(), 5);
    assert!(kinds.iter().any(|kind| kind["magic"] == "C011" && kind["count"] == 1));
    assert_eq!(value["details"]["kinds_truncated"], false);
}
