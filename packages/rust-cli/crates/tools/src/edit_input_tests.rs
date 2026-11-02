use serde_json::{Value, json};

use super::*;

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// 没给 `replace_all` 时写回 `false`，并排在最前（重建的对象字面量）。
#[test]
fn edit_without_replace_all_is_rebuilt_with_replace_all_first() {
    let input = json!({"file_path": "/a", "old_string": "x", "new_string": "y"});
    let rebuilt = rebuild(&input).expect("valid input");
    assert_eq!(
        keys(&rebuilt),
        ["replace_all", "file_path", "old_string", "new_string"]
    );
    assert_eq!(rebuilt["replace_all"], json!(false));
}

/// 模型给了 `replace_all`（在最后）也排到最前。
#[test]
fn edit_with_replace_all_moves_it_to_the_front() {
    let input =
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "replace_all": true});
    let rebuilt = rebuild(&input).expect("valid input");
    assert_eq!(
        keys(&rebuilt),
        ["replace_all", "file_path", "old_string", "new_string"]
    );
    assert_eq!(rebuilt["replace_all"], json!(true));
}

/// `fv` 是 `z.preprocess`：字符串 "true" / "false" 换成布尔。
#[test]
fn string_booleans_become_booleans() {
    let input =
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "replace_all": "true"});
    assert_eq!(rebuild(&input).expect("valid")["replace_all"], json!(true));
    let input =
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "replace_all": "false"});
    assert_eq!(rebuild(&input).expect("valid")["replace_all"], json!(false));
}

/// schema 解析失败时保留原输入：多余的键（严格对象）、类型不对、缺必填键、null。
#[test]
fn invalid_input_is_left_alone() {
    for input in [
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "extra": 1}),
        json!({"file_path": "/a", "old_string": 1, "new_string": "y"}),
        json!({"file_path": "/a", "new_string": "y"}),
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "replace_all": null}),
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "replace_all": "yes"}),
        json!("not an object"),
    ] {
        assert_eq!(rebuild(&input), None, "{input}");
    }
}

/// Edit 经 `Tool::rewrite_input` 交出这份整理（主循环不再按名字认 Edit，工作线 F131）；别的文件工具不整理。
#[test]
fn the_edit_tool_rewrites_through_the_tool_method() {
    use std::sync::Arc;

    use rust_cli_kernel_api::testing::FixedGate;
    use rust_cli_kernel_api::tool::Tool;

    use crate::common::FileTools;

    let files = || FileTools::new(Arc::default(), Arc::new(FixedGate::passthrough()));
    let input = json!({"file_path": "/a", "old_string": "x", "new_string": "y"});
    let rebuilt = crate::EditTool::new(files())
        .rewrite_input(&input)
        .expect("valid input");
    assert_eq!(
        keys(&rebuilt),
        ["replace_all", "file_path", "old_string", "new_string"]
    );
    assert_eq!(crate::WriteTool::new(files()).rewrite_input(&input), None);
}
