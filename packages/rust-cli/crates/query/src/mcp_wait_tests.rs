//! 直接调用连接中服务器的工具：等工具出现、预算、等满后不再等、重连中的不等（ext.md §3.8，修复线 F36）。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response, tool_use_response};
use serde_json::json;

use crate::mcp_support_tests::{FakeConnections, McpRegistry, TokioClock, server};
use crate::run::tests::{parts, request, scope};

const TOOL: &str = "mcp__slow__lookup";

fn results(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(user.content.iter().filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => {
                    Some(content.iter().filter_map(|item| match item {
                        ToolResultContent::Text { text } => Some(text.clone()),
                        _ => None,
                    }))
                }
                _ => None,
            })),
            _ => None,
        })
        .flatten()
        .flatten()
        .collect()
}

struct Ran {
    results: Vec<String>,
    elapsed: u64,
}

async fn run(
    registry: Arc<McpRegistry>,
    calls: usize,
    on_start: impl FnOnce(Arc<McpRegistry>),
) -> Ran {
    let mut script: Vec<_> = (0..calls)
        .map(|index| tool_use_response(&format!("toolu_{index}"), TOOL, &json!({})))
        .collect();
    script.push(text_response(&["done"]));
    let model = Arc::new(ScriptedModel::new(script));
    let clock = TokioClock::new();
    let mut parts = parts(model, Vec::new());
    parts.tools = registry.clone();
    parts.clock = clock.clone();
    let request = request("go");
    let (scope, sink) = scope(parts, &request, Abort::new());
    on_start(registry);
    let start = clock.now_ms();
    crate::run::run(scope, request).await.unwrap();
    Ran {
        results: results(&sink.messages()),
        elapsed: clock.now_ms() - start,
    }
}

fn lookup() -> Arc<dyn rust_cli_kernel_api::tool::Tool> {
    Arc::new(FakeTool::new(TOOL, true, "value for alpha"))
}

#[tokio::test(start_paused = true)]
async fn a_direct_call_waits_for_its_connecting_server() {
    let connections = FakeConnections::new(vec![server("slow", McpServerState::Pending)], 10_000);
    let registry = McpRegistry::new(Vec::new(), connections);
    let ran = run(registry, 1, |registry| {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
            registry.connect("slow", vec![lookup()]);
        });
    })
    .await;
    // 缺陷修复前回 `No such tool available`（应等服务器连上再执行）。
    assert_eq!(ran.results, ["value for alpha"]);
    assert_eq!(ran.elapsed, 1_500);
}

#[tokio::test(start_paused = true)]
async fn a_direct_call_gives_up_after_the_budget_and_does_not_wait_again() {
    let connections = FakeConnections::new(vec![server("slow", McpServerState::Pending)], 3_000);
    let registry = McpRegistry::new(Vec::new(), connections);
    let ran = run(registry, 2, |_| {}).await;
    let missing = format!("<tool_use_error>Error: No such tool available: {TOOL}</tool_use_error>");
    assert_eq!(ran.results, [missing.clone(), missing]);
    // 第一次等满 `min(MCP_TIMEOUT, 10 s)`；这个服务器本次运行不再等。
    assert_eq!(ran.elapsed, 3_000);
}

#[tokio::test(start_paused = true)]
async fn reconnecting_and_unknown_servers_are_not_waited_for() {
    let mut reconnecting = server("slow", McpServerState::Pending);
    reconnecting.reconnecting = true;
    let registry = McpRegistry::new(Vec::new(), FakeConnections::new(vec![reconnecting], 3_000));
    let ran = run(registry, 1, |_| {}).await;
    assert_eq!(ran.elapsed, 0);
    let registry = McpRegistry::new(
        Vec::new(),
        FakeConnections::new(vec![server("other", McpServerState::Pending)], 3_000),
    );
    let ran = run(registry, 1, |_| {}).await;
    assert_eq!(ran.elapsed, 0);
    assert!(ran.results[0].contains("No such tool available"));
}
