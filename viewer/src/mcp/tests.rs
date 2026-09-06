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

    let item_list = value["details"]["item_list"].as_array().unwrap();

    assert_eq!(item_list.len(), 5);
    assert_eq!(item_list[0]["magic"], "TMDH");
    assert_eq!(item_list[0]["kind"], "Header");
    assert_eq!(item_list[0]["duration"], 100);
    assert_eq!(item_list[1]["kind"], "ActorList");
    assert_eq!(item_list[1]["actors"], serde_json::json!([1]));
    assert_eq!(item_list[2]["kind"], "Actor");
    assert_eq!(item_list[2]["tracks"], serde_json::json!([2]));
    assert_eq!(item_list[3]["kind"], "Track");
    assert_eq!(item_list[3]["commands"], serde_json::json!([3]));
    assert_eq!(item_list[4]["kind"], "Command");
    assert_eq!(item_list[4]["id"], 3);
    assert_eq!(item_list[4]["time"], 5);
    assert_eq!(item_list[4]["command"]["magic"], "C011");
    assert_eq!(item_list[4]["command"]["enabled"], 1);
    assert_eq!(item_list[4]["command"]["unknown_2"], 0);
}

/// A `.uld` carrying one component holding an image and a text node, one widget with a resource
/// node, and one timeline of an alpha animation plus a label set — the same nesting layout.rs's
/// own tests build.
fn uld() -> Vec<u8> {
    const HEADER: usize = 16;
    const SECTION: usize = 36;

    fn section(offsets: &[(usize, u32)]) -> Vec<u8> {
        let mut out = Vec::from(*b"atkh0100");
        let mut slots = [0u32; 7];
        for (slot, offset) in offsets {
            slots[*slot] = *offset;
        }
        out.extend(slots.iter().flat_map(|offset| offset.to_le_bytes()));
        out
    }

    fn list(magic: &[u8; 4], count: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::from(*magic);
        out.extend_from_slice(b"0100");
        out.extend(count.to_le_bytes());
        out.extend(0i32.to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn node(id: u32, parent: i32, node_type: i32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(id.to_le_bytes());
        out.extend(parent.to_le_bytes());
        out.extend([0u8; 12]);
        out.extend(node_type.to_le_bytes());
        out.extend(
            u16::try_from(88 + payload.len())
                .unwrap()
                .to_le_bytes(),
        );
        out.resize(88, 0);
        out.extend_from_slice(payload);
        out
    }

    let mut image = Vec::new();
    image.extend(1u32.to_le_bytes());
    image.extend(0u32.to_le_bytes());
    image.extend([0u8; 4]);

    let mut text = Vec::new();
    text.extend(5u32.to_le_bytes());
    text.extend(u32::MAX.to_le_bytes());
    text.push(4);
    text.push(0);
    text.push(0);
    text.push(12);
    text.extend(0u32.to_le_bytes());
    text.push(0);
    text.push(0);
    text.push(0);
    text.push(0);
    text.push(2);
    text.extend([0u8; 3]);

    let mut component = Vec::new();
    component.extend(1001u32.to_le_bytes());
    component.extend([0u8; 3]);
    component.push(8);
    component.extend(2u32.to_le_bytes());
    component.extend(228u16.to_le_bytes());
    component.extend(16u16.to_le_bytes());
    component.extend(node(1, 0, 2, &image));
    component.extend(node(2, 0, 3, &text));

    let mut alpha = Vec::new();
    alpha.extend(3u16.to_le_bytes());
    alpha.extend(6u16.to_le_bytes());
    alpha.extend(12u16.to_le_bytes());
    alpha.extend(4u16.to_le_bytes());
    alpha.extend([0xDE, 0xAD, 0xBE, 0xEF]);

    let mut animation = Vec::new();
    animation.extend(1u32.to_le_bytes());
    animation.extend(59u32.to_le_bytes());
    animation.extend(28u32.to_le_bytes());
    animation.extend(1u32.to_le_bytes());
    animation.extend(alpha);

    let mut label = Vec::new();
    label.extend(0u16.to_le_bytes());
    label.extend(0x19u16.to_le_bytes());
    label.extend(12u16.to_le_bytes());
    label.extend(4u16.to_le_bytes());
    label.extend([0u8; 4]);

    let mut label_set = Vec::new();
    label_set.extend(0u32.to_le_bytes());
    label_set.extend(0u32.to_le_bytes());
    label_set.extend(28u32.to_le_bytes());
    label_set.extend(1u32.to_le_bytes());
    label_set.extend(label);

    let mut timeline = Vec::new();
    timeline.extend(7u32.to_le_bytes());
    timeline.extend(68u32.to_le_bytes());
    timeline.extend(1u16.to_le_bytes());
    timeline.extend(1u16.to_le_bytes());
    timeline.extend(animation);
    timeline.extend(label_set);

    let mut widget = Vec::new();
    widget.extend(1u32.to_le_bytes());
    widget.push(4);
    widget.extend([0u8; 2]);
    widget.push(1);
    widget.extend(10i16.to_le_bytes());
    widget.extend(20i16.to_le_bytes());
    widget.extend(1u16.to_le_bytes());
    widget.extend(104u16.to_le_bytes());
    widget.extend(node(1, 0, 1, &[]));

    let component_list = list(b"cohd", 1, &component);
    let timeline_list = list(b"tlhd", 1, &timeline);
    let widget_list = list(b"wdhd", 1, &widget);

    let timeline_offset = u32::try_from(SECTION + component_list.len()).unwrap();
    let widget_section_at = u32::try_from(
        HEADER + SECTION + component_list.len() + timeline_list.len(),
    )
    .unwrap();

    let mut out = Vec::new();
    out.extend(b"uldh0100");
    out.extend(16u32.to_le_bytes());
    out.extend(widget_section_at.to_le_bytes());
    out.extend(section(&[(2, 36), (3, timeline_offset)]));
    out.extend(component_list);
    out.extend(timeline_list);
    out.extend(section(&[(4, 36)]));
    out.extend(widget_list);
    out
}

#[test]
fn inspect_structures_a_uld_layout() {
    use super::assets::inspect;

    let value: serde_json::Value =
        serde_json::from_str(&inspect("ui/uld/example.uld", &uld(), 100).unwrap()).unwrap();

    assert_eq!(value["format"]["viewer"], "布局");
    assert_eq!(value["details"]["version"], "0100");
    assert_eq!(value["details"]["component_count"], 1);
    let component = &value["details"]["components"][0];
    assert_eq!(component["kind"], "NumericInput");
    assert_eq!(component["node_count"], 2);
    assert_eq!(component["nodes"][0]["kind"]["name"], "Image");
    assert_eq!(component["nodes"][0]["kind"]["part_list_id"], 1);
    assert_eq!(component["nodes"][1]["kind"]["name"], "Text");
    assert_eq!(component["nodes"][1]["kind"]["text_id"], 5);
    assert_eq!(component["nodes"][1]["kind"]["font"], "Axis");
    assert_eq!(component["nodes"][1]["kind"]["font_size"], 12);

    let widget = &value["details"]["widgets"][0];
    assert_eq!(widget["x"], 10);
    assert_eq!(widget["y"], 20);
    assert_eq!(widget["nodes"][0]["kind"]["name"], "Res");

    let timeline = &value["details"]["timelines"][0];
    assert_eq!(timeline["animation_count"], 1);
    assert_eq!(timeline["label_set_count"], 1);
    let group = &timeline["animations"][0]["groups"][0];
    assert_eq!(group["usage"], "Alpha");
    assert_eq!(group["kind"], "Byte1");
    assert_eq!(group["keyframe_count"], 4);
    assert_eq!(group["keyframe_size"], 1);
    assert_eq!(group["data_hex"], "DEADBEEF");
    assert_eq!(timeline["label_sets"][0]["groups"][0]["kind"], "Label");

    assert_eq!(value["details"]["truncated"]["components"], false);
    assert_eq!(value["details"]["truncated"]["timelines"], false);
}
