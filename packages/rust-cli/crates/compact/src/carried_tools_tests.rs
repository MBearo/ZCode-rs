//! 压缩前已加载的工具记在边界上（docs/specs/rust-cli/compact.md §3.5）。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, SystemMessage, ToolResultContent,
};
use tokio_util::sync::CancellationToken;

use super::Engine;
use crate::test_support::{
    Reply, ScriptedRunner, human, no_tools, ports, reply, request, tool_round,
};

fn summary() -> Reply {
    Reply::Text("<analysis>a</analysis>\n<summary>S</summary>".to_owned())
}

/// 一轮工具调用，结果里引用了 `name`（ToolSearch 加载它）。
fn loading_round(index: usize, name: &str) -> [Message; 2] {
    let [assistant, mut result] = tool_round(index, "ToolSearch", "");
    if let Message::User(user) = &mut result
        && let Some(ContentBlock::ToolResult { content, .. }) = user.content.first_mut()
    {
        *content = vec![ToolResultContent::ToolReference {
            tool_name: name.to_owned(),
        }];
    }
    [assistant, result]
}

fn earlier_boundary(carried: &[&str]) -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("b0"),
        trigger: CompactTrigger::Auto,
        pre_tokens: 1,
        summary: MessageId::new("s0"),
        preserved: None,
        discovered_tools: carried.iter().map(|name| (*name).to_owned()).collect(),
    }))
}

async fn compact(history: Vec<Message>) -> CompactBoundary {
    let runner = ScriptedRunner::new(vec![summary()]);
    let ports = ports(runner, no_tools());
    Engine { ports: &ports }
        .run(
            &request(history, CompactTrigger::Auto),
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .boundary
}

#[tokio::test]
async fn the_boundary_records_the_tools_loaded_before_compaction() {
    let mut history = vec![human("u1", "first")];
    history.extend(loading_round(1, "mcp__fixture__lookup"));
    history.extend([
        reply("a1", "one", None),
        human("u2", "second"),
        reply("a2", "two", None),
    ]);
    let boundary = compact(history).await;
    assert_eq!(boundary.discovered_tools, ["mcp__fixture__lookup"]);
}

#[tokio::test]
async fn tools_carried_by_the_last_boundary_stay_loaded() {
    let mut history = vec![
        human("u0", "before"),
        earlier_boundary(&["WebFetch"]),
        human("u1", "first"),
    ];
    history.extend(loading_round(1, "NotebookEdit"));
    history.extend([
        reply("a1", "one", None),
        human("u2", "second"),
        reply("a2", "two", None),
    ]);
    let boundary = compact(history).await;
    assert_eq!(boundary.discovered_tools, ["NotebookEdit", "WebFetch"]);
}

#[tokio::test]
async fn nothing_is_recorded_when_no_tool_was_loaded() {
    let history = vec![
        human("u1", "first"),
        reply("a1", "one", None),
        human("u2", "second"),
        reply("a2", "two", None),
    ];
    assert!(compact(history).await.discovered_tools.is_empty());
}
