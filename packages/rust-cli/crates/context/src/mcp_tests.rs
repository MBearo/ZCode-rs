//! `mcp_instructions_delta` 的增量与文本（ext.md §3.6）；新增一段的全文是固定的。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::AttachmentMessage;

use super::*;

fn entry(name: &str, text: &str) -> ListingEntry {
    ListingEntry {
        name: name.to_owned(),
        line: format!("## {name}\n{text}"),
    }
}

fn announced_as(data: Value) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("m1"),
        source: SOURCE.to_owned(),
        text: String::new(),
        data,
    })
}

#[test]
fn m1_announces_new_servers() {
    let current = [entry(
        "fixture",
        "Use lookup to read stored values. Never call explode on purpose.",
    )];
    let (text, data) = announce(&[], &current).unwrap();
    assert_eq!(
        text,
        "# MCP Server Instructions\n\nThe following MCP servers have provided instructions for how to use their tools and resources:\n\n## fixture\nUse lookup to read stored values. Never call explode on purpose."
    );
    assert_eq!(data["addedNames"], json!(["fixture"]));
    // 已宣布且没变：不再宣布。
    assert_eq!(announce(&[announced_as(data)], &current), None);
}

#[test]
fn changed_and_disconnected_servers() {
    let history = [announced_as(json!({
        "addedNames": ["a", "b"],
        "addedBlocks": ["## a\nold", "## b\nB"],
        "removedNames": []
    }))];
    let (text, data) = announce(&history, &[entry("a", "new")]).unwrap();
    assert!(text.starts_with("# MCP Server Instructions"));
    assert!(text.contains("## a\nnew\n\nThe following MCP servers have disconnected. Their instructions above no longer apply:\nb\n\nThis is ambient context"));
    assert_eq!(data["removedNames"], json!(["b"]));
}
