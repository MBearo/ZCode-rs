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
    let rebuilt = edit(&input).expect("valid input");
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
    let rebuilt = edit(&input).expect("valid input");
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
    assert_eq!(edit(&input).expect("valid")["replace_all"], json!(true));
    let input =
        json!({"file_path": "/a", "old_string": "x", "new_string": "y", "replace_all": "false"});
    assert_eq!(edit(&input).expect("valid")["replace_all"], json!(false));
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
        assert_eq!(edit(&input), None, "{input}");
    }
}

/// 只整理 Edit；ToolSearch 的 `max_results` 不补。
#[test]
fn only_edit_is_rewritten() {
    struct Named(&'static str);
    #[async_trait::async_trait]
    impl rust_cli_kernel_api::tool::Tool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object", "properties": {"n": {"type": "number", "default": 5}}})
        }
        fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
            String::new()
        }
        async fn call(
            &self,
            _input: Value,
            _context: rust_cli_kernel_api::tool::ToolUseContext,
            _progress: tokio::sync::mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
        ) -> Result<rust_cli_kernel_api::tool::ToolOutput, rust_cli_kernel_api::tool::ToolError>
        {
            unreachable!("not called")
        }
    }
    let tools = rust_cli_kernel_api::testing::ListTools(vec![
        std::sync::Arc::new(Named("Edit")),
        std::sync::Arc::new(Named("ToolSearch")),
    ]);
    let block = |name: &str, input: Value| ContentBlock::ToolUse {
        id: rust_cli_kernel_api::ids::ToolUseId::new("t"),
        name: name.to_owned(),
        input,
    };
    let given = json!({"file_path": "/a", "old_string": "x", "new_string": "y"});
    let mut edit_block = block("Edit", given);
    apply(&tools, &mut edit_block);
    let ContentBlock::ToolUse { input, .. } = &edit_block else {
        unreachable!()
    };
    assert_eq!(
        keys(input),
        ["replace_all", "file_path", "old_string", "new_string"]
    );
    let mut search = block("ToolSearch", json!({}));
    apply(&tools, &mut search);
    assert!(matches!(search, ContentBlock::ToolUse { input, .. } if input == json!({})));
}
