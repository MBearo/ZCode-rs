use serde_json::json;

use super::*;

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": {"type": "string"},
            "offset": {"type": "number"},
            "limit": {"type": "number"}
        },
        "required": ["file_path"],
        "additionalProperties": false
    })
}

#[test]
fn valid_input_passes() {
    assert_eq!(
        validate("Read", &schema(), &json!({"file_path": "a"})),
        None
    );
}

#[test]
fn a_single_missing_parameter_uses_the_singular() {
    assert_eq!(
        validate("Read", &schema(), &json!({})).unwrap(),
        "Read failed due to the following issue:\nThe required parameter `file_path` is missing"
    );
}

#[test]
fn issues_are_grouped_missing_unexpected_then_type() {
    let message = validate("Read", &schema(), &json!({"offset": "1", "extra": true})).unwrap();
    assert_eq!(
        message,
        "Read failed due to the following issues:\n\
         The required parameter `file_path` is missing\n\
         An unexpected parameter `extra` was provided\n\
         The parameter `offset` type is expected as `number` but provided as `string`"
    );
}

#[test]
fn nested_paths_use_dots_and_indexes() {
    let schema = json!({
        "type": "object",
        "properties": {
            "todos": {"type": "array", "items": {
                "type": "object",
                "properties": {"content": {"type": "string"}},
                "required": ["content"]
            }}
        }
    });
    let message = validate("TodoWrite", &schema, &json!({"todos": [{"content": 1}]})).unwrap();
    assert!(
        message.contains(
            "The parameter `todos[0].content` type is expected as `string` but provided as `number`"
        ),
        "{message}"
    );
}

#[test]
fn required_fields_with_a_default_may_be_omitted() {
    // ToolSearch 的 schema：`max_results` 有默认值，仍列在 required 里。
    let schema = json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "max_results": {"default": 5, "type": "number"}
        },
        "required": ["query", "max_results"],
        "additionalProperties": false
    });
    assert_eq!(
        validate("ToolSearch", &schema, &json!({"query": "x"})),
        None
    );
    assert!(validate("ToolSearch", &schema, &json!({"max_results": 2})).is_some());
}
