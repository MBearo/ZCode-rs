//! MCP 名字规范化（ext.md §3.4）。

use super::*;

#[test]
fn names_keep_only_api_safe_characters() {
    assert_eq!(normalize_name("my server.v2"), "my_server_v2");
    assert_eq!(tool_name("files", "read-file"), "mcp__files__read-file");
    assert_eq!(
        tool_name("plugin:kit:db", "query"),
        "mcp__plugin_kit_db__query"
    );
    assert_eq!(McpSettings::default().max_output_tokens, 25_000);
}
