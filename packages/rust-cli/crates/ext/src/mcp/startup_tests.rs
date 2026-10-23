//! `-p` 启动等待的设置：截止时刻的优先级、allow 规则点名的服务器（ext.md §3.3.1，修复线 F36）。

use super::*;

#[test]
fn the_startup_wait_variable_wins_then_an_explicit_config_then_two_seconds() {
    // ZCODE_MCP_STARTUP_WAIT_MS 优先，0 也算（不等）。
    assert_eq!(prewait_ms(Some(750), true, 30_000), 750);
    assert_eq!(prewait_ms(Some(0), false, 30_000), 0);
    // 没设时：显式给了 `--mcp-config` 的服务器用 MCP_TIMEOUT。
    assert_eq!(prewait_ms(None, true, 30_000), 30_000);
    assert_eq!(prewait_ms(None, true, 4_000), 4_000);
    // 否则 2000。
    assert_eq!(prewait_ms(None, false, 30_000), 2_000);
}

#[test]
fn the_defaults_are_non_blocking_five_and_two_seconds() {
    let startup = McpStartup::default();
    assert!(startup.nonblocking);
    assert_eq!(startup.connect_wait_ms, 5_000);
    assert_eq!(startup.prewait_ms, 2_000);
    assert!(startup.required.is_empty());
}

#[test]
fn allow_rules_name_servers_by_their_normalized_segment() {
    let required = required_servers([
        "mcp__slow",
        "mcp__files__read",
        "mcp__my server(arg)",
        "Bash(sleep:*)",
        "mcp__",
        "mcpx__nope",
        " mcp__spaced ",
    ]);
    let names: Vec<&str> = required.iter().map(String::as_str).collect();
    assert_eq!(names, ["files", "my_server", "slow", "spaced"]);
}

#[test]
fn a_server_segment_stops_at_the_first_double_underscore() {
    assert_eq!(segment("slow"), "slow");
    assert_eq!(segment("my server"), "my_server");
    assert_eq!(segment("a__b"), "a");
    // 第一段为空时用整个规范化的名字。
    assert_eq!(segment("__x"), "__x");
}

/// `firstTurnHold`（headless.md §6，修复线 F39）：只有 stream-json 输入、能用工具搜索、没有显式
/// `--mcp-config`、没有设 `ZCODE_MCP_STARTUP_WAIT_MS`（原名不读）时是 `lifted`（不等可延迟的服务器）。
#[test]
fn only_streaming_input_with_tool_search_lifts_the_wait() {
    assert!(!wait_for_deferrable(true, true, false, None));
    // single_shot：单次 `-p "…"`。
    assert!(wait_for_deferrable(false, true, false, None));
    // no_tool_search。
    assert!(wait_for_deferrable(true, false, false, None));
    // explicit_mcp_config。
    assert!(wait_for_deferrable(true, true, true, None));
    // host_wait：设了等待时间。
    assert!(wait_for_deferrable(true, true, false, Some(500)));
    assert!(McpStartup::default().wait_for_deferrable);
}
