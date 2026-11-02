use super::{McpName, compose, is_mcp, parse, server_prefix};

#[test]
fn compose_and_parse_round_trip() {
    let name = compose("files", "read");
    assert_eq!(name, "mcp__files__read");
    assert!(name.starts_with(&server_prefix("files")));
    assert_eq!(
        parse(&name),
        Some(McpName {
            server: "files",
            tool: Some("read")
        })
    );
    assert!(is_mcp(&name));
    assert!(!is_mcp("Read"));
}

/// 第一个 `__` 之前是服务器；其余是工具（可能还含 `__`，可能没有）。
#[test]
fn the_first_separator_splits_server_and_tool() {
    assert_eq!(
        parse("mcp__a__b__c"),
        Some(McpName {
            server: "a",
            tool: Some("b__c")
        })
    );
    assert_eq!(
        parse("mcp__a"),
        Some(McpName {
            server: "a",
            tool: None
        })
    );
    assert_eq!(
        parse("mcp__"),
        Some(McpName {
            server: "",
            tool: None
        })
    );
    assert_eq!(
        parse("mcp__mcp__read"),
        Some(McpName {
            server: "mcp",
            tool: Some("read")
        })
    );
    assert_eq!(parse("mcpx__a"), None);
    assert_eq!(parse("Bash"), None);
}
