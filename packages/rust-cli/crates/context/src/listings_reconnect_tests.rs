//! 延迟工具的撤回与重新可用（docs/specs/rust-cli/context.md §4.3）：MCP 服务器断开与重连的文字。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use serde_json::{Value, json};

use super::*;

fn tools(names: &[&str]) -> Vec<ListingEntry> {
    names
        .iter()
        .map(|name| ListingEntry {
            name: (*name).to_owned(),
            line: (*name).to_owned(),
        })
        .collect()
}

fn recorded((_, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("a"),
        source: DEFERRED_TOOLS.to_owned(),
        text: String::new(),
        data,
    })
}

const AMBIENT: &str = "This is ambient context — do not narrate it to the user unless they ask or it is directly relevant to their request.";

/// 一步：按这一步的工具算增量，记进历史。
fn step(history: &mut Vec<Message>, now: &[&str]) -> Option<(String, Value)> {
    let found = deferred_tools(history, &tools(now), None);
    if let Some(found) = &found {
        history.push(recorded(found.clone()));
    }
    found
}

const FIXTURE: [&str; 4] = [
    "mcp__fixture__explode",
    "mcp__fixture__lookup",
    "mcp__fixture__quit",
    "mcp__fixture__rich",
];

#[test]
fn a_disconnected_server_is_named_and_its_return_is_not_listed_again() {
    let mut history = Vec::new();
    let mut all = vec!["WebFetch"];
    all.extend(FIXTURE);
    step(&mut history, &all).unwrap();

    // 断开：MCP 工具按“服务器断开”的模板撤回。
    let (text, data) = step(&mut history, &["WebFetch"]).unwrap();
    assert_eq!(
        text,
        format!(
            "The following deferred tools are no longer available (their MCP server disconnected). Do not search for them — ToolSearch will return no match:\n{}\n\n{AMBIENT}",
            FIXTURE.join("\n")
        )
    );
    assert_eq!(data["removedNames"], json!(FIXTURE));

    // 重连：只说重新可用（按服务器归组），不再列行。
    let (text, data) = step(&mut history, &all).unwrap();
    assert_eq!(
        text,
        "4 deferred tools are available again (MCP server reconnected — names announced earlier in this conversation): mcp__fixture__* (4). Load via ToolSearch as before."
    );
    assert_eq!(data["readdedNames"], json!(FIXTURE));
    assert_eq!(data["addedNames"], json!(FIXTURE));
    assert_eq!(data["addedLines"], json!([]));
    // 之后没有变化。
    assert!(step(&mut history, &all).is_none());
}

#[test]
fn other_tools_that_come_back_are_named_one_by_one() {
    let mut history = Vec::new();
    step(&mut history, &["NotebookEdit", "WebFetch", "mcp__a__x"]).unwrap();
    let (text, _) = step(&mut history, &[]).unwrap();
    assert!(text.starts_with(
        "The following deferred tools are no longer available (their MCP server disconnected). Do not search for them — ToolSearch will return no match:\nmcp__a__x\n\nThe following deferred tools are no longer available in this session. Do not search for them — ToolSearch will return no match:\nNotebookEdit\nWebFetch\n\n"
    ));
    let (text, _) = step(&mut history, &["NotebookEdit", "mcp__a__x", "Zed"]).unwrap();
    assert_eq!(
        text,
        "The following deferred tools are now available via ToolSearch. Their schemas are NOT loaded — calling them directly will fail with InputValidationError. Use ToolSearch with query \"select:<name>[,<name>...]\" to load tool schemas before calling them:\nZed\n\n1 deferred tool is available again (MCP server reconnected — names announced earlier in this conversation): mcp__a__*. Load via ToolSearch as before.\n\n1 deferred tool is available again in this session (announced earlier in this conversation): NotebookEdit. Load via ToolSearch as before."
    );
}

#[test]
fn more_than_thirty_disconnected_tools_are_summarized_by_server() {
    let names: Vec<String> = (0..31)
        .map(|index| format!("mcp__big__t{index:02}"))
        .chain(["mcp__solo__only".to_owned()])
        .collect();
    let all: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut history = Vec::new();
    step(&mut history, &all).unwrap();
    let (text, _) = step(&mut history, &[]).unwrap();
    assert_eq!(
        text,
        format!(
            "32 deferred tools are no longer available (MCP server disconnected): mcp__big__* (31), mcp__solo__*. Do not search for them — ToolSearch will return no match.\n\n{AMBIENT}"
        )
    );
}
