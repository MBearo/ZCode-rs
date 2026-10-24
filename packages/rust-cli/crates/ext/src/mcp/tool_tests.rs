//! 工具自己的不延迟标记（docs/specs/rust-cli/ext.md §3.1、D35）：`_meta` 里新旧两个键任一为 true 就不延迟。
use std::sync::Weak;

use serde_json::{Value, json};

use super::McpTool;

fn always_load(listed: &Value, server_always_load: bool) -> bool {
    McpTool::from_listed("files", listed, server_always_load, Weak::new())
        .unwrap()
        .info
        .always_load
}

fn with_meta(meta: Value) -> Value {
    json!({"name": "read", "inputSchema": {"type": "object"}, "_meta": meta})
}

#[test]
fn either_meta_key_keeps_the_tool_loaded() {
    assert!(always_load(
        &with_meta(json!({"zcode/alwaysLoad": true})),
        false
    ));
    // 旧键：Node 与已有的 MCP 服务器用它。
    assert!(always_load(
        &with_meta(json!({"anthropic/alwaysLoad": true})),
        false
    ));
}

#[test]
fn other_values_and_keys_leave_the_tool_deferred() {
    for meta in [
        json!({"anthropic/alwaysLoad": false}),
        json!({"anthropic/alwaysLoad": "true"}),
        json!({"zcode/alwaysLoad": 1}),
        json!({"other/alwaysLoad": true}),
        json!({"alwaysLoad": true}),
        json!(null),
    ] {
        assert!(!always_load(&with_meta(meta.clone()), false), "{meta}");
    }
    assert!(!always_load(&json!({"name": "read"}), false));
}

#[test]
fn the_server_option_does_not_look_at_the_tool() {
    assert!(always_load(&json!({"name": "read"}), true));
    assert!(always_load(
        &with_meta(json!({"anthropic/alwaysLoad": false})),
        true
    ));
}
