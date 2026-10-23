use serde_json::json;

use super::*;

#[test]
fn the_marker_round_trips_and_other_objects_are_not_markers() {
    let value = marker("{\"a\": 1");
    assert_eq!(
        value,
        json!({"__unparsedToolInput": {"raw": "{\"a\": 1", "len": 7}})
    );
    assert_eq!(
        parse(&value),
        Some(Unparsed {
            raw: "{\"a\": 1",
            len: 7
        })
    );
    assert_eq!(parse(&json!({"command": "ls"})), None);
    assert_eq!(
        parse(&json!({"__unparsedToolInput": {"raw": "x", "len": 1}, "other": 1})),
        None
    );
    assert_eq!(
        parse(&json!({"__unparsedToolInput": {"raw": 1, "len": 1}})),
        None
    );
}

#[test]
fn the_raw_text_keeps_a_utf16_prefix() {
    let raw = format!("{}😀", "a".repeat(RAW_UNITS - 1));
    let value = marker(&raw);
    assert_eq!(value[KEY]["raw"], json!("a".repeat(RAW_UNITS - 1)));
    assert_eq!(value[KEY]["len"], json!(RAW_UNITS + 1));
}
