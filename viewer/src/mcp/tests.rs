use serde_json::json;

use super::{
    handler::{QueryRowsParams, SearchCellsParams},
    process_validate_filter,
};

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
