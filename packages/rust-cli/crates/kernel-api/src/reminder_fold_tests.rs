use super::*;
use crate::ids::ToolUseId;
use crate::model::CacheControl;

fn result(content: Vec<ToolResultContent>, is_error: bool) -> ApiContent {
    ApiContent {
        block: ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("toolu_1"),
            content,
            is_error,
        },
        cache_control: None,
    }
}

fn text(value: &str) -> ToolResultContent {
    ToolResultContent::Text {
        text: value.to_owned(),
    }
}

fn plain(value: &str) -> ApiContent {
    ApiContent {
        block: ContentBlock::Text {
            text: value.to_owned(),
        },
        cache_control: None,
    }
}

fn inner(content: &[ApiContent]) -> Vec<ToolResultContent> {
    match &content.last().map(|item| &item.block) {
        Some(ContentBlock::ToolResult { content, .. }) => content.clone(),
        other => panic!("last block is not a tool result: {other:?}"),
    }
}

const REMINDER: &str =
    "<system-reminder>\n<total_tokens>9 tokens left</total_tokens>\n</system-reminder>";

const STRING: fn(&ToolUseId, bool) -> bool = |_, _| false;
const ARRAY: fn(&ToolUseId, bool) -> bool = |_, _| true;

/// 字符串结果（Read，没有对话中间 system 的模型）：去掉首尾空白，空行接上提醒。
#[test]
fn a_string_result_takes_the_trimmed_reminder_after_a_blank_line() {
    let mut content = vec![result(vec![text("1\talpha\n2\tbeta\n3\t")], false)];
    assert!(fold_into_tool_result(&mut content, REMINDER, STRING));
    assert_eq!(content.len(), 1);
    assert_eq!(
        inner(&content),
        [text(&format!("1\talpha\n2\tbeta\n3\n\n{REMINDER}"))]
    );
}

#[test]
fn an_empty_string_result_becomes_the_reminder() {
    let mut content = vec![result(vec![text("  \n")], false)];
    assert!(fold_into_tool_result(&mut content, REMINDER, STRING));
    assert_eq!(inner(&content), [text(REMINDER)]);
    let mut empty = vec![result(Vec::new(), false)];
    assert!(fold_into_tool_result(&mut empty, REMINDER, STRING));
    assert_eq!(inner(&empty), [text(REMINDER)]);
}

/// JS 的 String.prototype.trim 去掉 U+FEFF、不去 U+0085（与 Rust 的 `str::trim` 不同）。
#[test]
fn trimming_follows_javascript() {
    let mut content = vec![result(vec![text("\u{feff}ok\u{85}")], true)];
    assert!(fold_into_tool_result(&mut content, REMINDER, STRING));
    assert_eq!(inner(&content), [text(&format!("ok\u{85}\n\n{REMINDER}"))]);
}

/// 块数组结果（Agent、MCP 工具）：最后一个文本块补一个换行，提醒成为结果里的下一个块（不去空白）。
#[test]
fn an_array_result_takes_the_reminder_as_another_block() {
    let mut content = vec![result(vec![text("done")], false)];
    assert!(fold_into_tool_result(&mut content, REMINDER, ARRAY));
    assert_eq!(inner(&content), [text("done\n"), text(REMINDER)]);
    let mut several = vec![result(vec![text("a"), text("b")], false)];
    assert!(fold_into_tool_result(&mut several, REMINDER, STRING));
    assert_eq!(inner(&several), [text("a"), text("b\n"), text(REMINDER)]);
}

#[test]
fn tool_references_and_trailing_text_keep_the_reminder_outside() {
    let reference = ToolResultContent::ToolReference {
        tool_name: "WebFetch".to_owned(),
    };
    let mut content = vec![result(vec![reference], false)];
    assert!(!fold_into_tool_result(&mut content, REMINDER, STRING));
    let mut after_text = vec![result(vec![text("ok")], false), plain("Tool loaded.")];
    assert!(!fold_into_tool_result(&mut after_text, REMINDER, STRING));
    assert!(!fold_into_tool_result(&mut [], REMINDER, STRING));
}

/// 署名提醒留在结果之外。
#[test]
fn the_attribution_reminder_stays_a_sibling() {
    let mut content = vec![result(vec![text("ok")], false)];
    let attribution = "<system-reminder>\nAttribution for git commits and pull requests you create from here on (…)\n</system-reminder>";
    assert!(!fold_into_tool_result(&mut content, attribution, STRING));
    let none = "<system-reminder>\nFrom here on, do not add attribution lines to git commit messages or pull request descriptions.\n</system-reminder>";
    assert!(!fold_into_tool_result(&mut content, none, STRING));
    assert_eq!(inner(&content), [text("ok")]);
}

#[test]
fn the_cache_breakpoint_on_the_result_is_kept() {
    let mut content = vec![ApiContent {
        cache_control: Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: None,
        }),
        ..result(vec![text("ok")], false)
    }];
    assert!(fold_into_tool_result(&mut content, REMINDER, STRING));
    assert!(content[0].cache_control.is_some());
}

#[test]
fn array_results_are_agent_send_message_and_mcp_tools_unless_they_failed() {
    assert!(array_result("Agent", false));
    assert!(array_result("SendMessage", false));
    assert!(array_result("mcp__fixture__lookup", false));
    assert!(!array_result("mcp__fixture__lookup", true));
    assert!(!array_result("Read", false));
    assert!(!array_result("Bash", false));
}
