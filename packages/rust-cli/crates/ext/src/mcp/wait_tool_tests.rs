//! WaitForMcpServers：要等的服务器、5 s 上限、分类与结果文字（ext.md §3.8，修复线 F36）。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::MemoryEnvironment;
use serde_json::json;

use super::*;
use crate::clock_tests::TokioClock;
use crate::mcp::config::McpSettings;
use crate::mcp::{McpScope, McpServer};
use crate::test_support::{FakeMcp, held, server, tool_context, workspace};

fn config(name: &str) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::Project,
        transport: McpTransportConfig::Stdio {
            command: "node".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
        },
        options: Default::default(),
    }
}

struct Rig {
    fake: Arc<FakeMcp>,
    manager: Arc<McpManager>,
    clock: Arc<TokioClock>,
    _scope: TaskScope,
}

fn rig(fake: FakeMcp, servers: &[&str]) -> Rig {
    let fake = Arc::new(fake);
    let scope = TaskScope::root("test", |_| {});
    let clock = TokioClock::new();
    let manager = Arc::new(McpManager::new(
        fake.clone(),
        scope.spawner(),
        McpSettings::default(),
        clock.clone(),
    ));
    manager.start(
        workspace(),
        servers.iter().map(|name| config(name)).collect(),
    );
    Rig {
        fake,
        manager,
        clock,
        _scope: scope,
    }
}

async fn call(rig: &Rig, input: serde_json::Value) -> (ToolOutput, u64) {
    let tool = WaitForMcpServers::new(Arc::downgrade(&rig.manager));
    let context = tool_context(
        Arc::new(MemoryEnvironment::new(crate::test_support::facts())),
        PermissionMode::Default,
    );
    let (progress, _) = mpsc::channel(1);
    let start = rig.clock.now_ms();
    let output = tool.call(input, context, progress).await.unwrap();
    (output, rig.clock.now_ms() - start)
}

fn text(output: &ToolOutput) -> &str {
    match &output.content[..] {
        [ToolResultContent::Text { text }] => text,
        other => panic!("unexpected content {other:?}"),
    }
}

fn ready() -> Arc<crate::test_support::Handler> {
    server(json!({}), None, |_, _| None)
}

#[test]
fn the_definition_comes_from_the_prompt_data() {
    let tool = WaitForMcpServers::new(std::sync::Weak::new());
    let prompt = PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    };
    assert!(
        tool.description(&prompt)
            .starts_with("Wait for MCP servers that are still connecting")
    );
    assert_eq!(
        tool.input_schema()["properties"]["servers"]["description"],
        "Server names to wait for (default: all pending)"
    );
    assert!(tool.is_read_only(&json!({})));
    assert!(!tool.is_concurrency_safe(&json!({})));
}

#[tokio::test(start_paused = true)]
async fn waits_for_the_pending_servers_then_reports_them_connected() {
    let rig = rig(
        FakeMcp::default()
            .with("slow", held(|_, _| None))
            .with("fast", ready()),
        &["slow", "fast"],
    );
    // fast 先连上，只剩 slow 在连。
    let only_slow = |view: &McpView| view.pending() == ["slow"];
    rig.manager.wait(1_000, &only_slow).await;
    let fake = rig.fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
        fake.release("slow", json!({}), None);
    });
    let (output, waited) = call(&rig, json!({})).await;
    assert_eq!(waited, 1_500);
    assert!(!output.is_error);
    assert_eq!(
        text(&output),
        "ready: true\nConnected (their tools are now available \u{2014} call them directly): slow"
    );
}

#[tokio::test(start_paused = true)]
async fn gives_up_after_five_seconds() {
    let rig = rig(
        FakeMcp::default().with("slow", held(|_, _| None)),
        &["slow"],
    );
    let (output, waited) = call(&rig, json!({})).await;
    assert_eq!(waited, 5_000);
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        "ready: false\nStill connecting (try again or proceed without): slow"
    );
}

#[tokio::test(start_paused = true)]
async fn without_pending_servers_it_reports_the_failed_ones() {
    let rig = rig(
        FakeMcp::default().with("fast", ready()).refusing("broken"),
        &["fast", "broken"],
    );
    rig.manager.wait_ready().await;
    let (output, waited) = call(&rig, json!({})).await;
    assert_eq!(waited, 0);
    assert!(output.is_error);
    assert_eq!(text(&output), "ready: false\nFailed to connect: broken");
}

#[tokio::test(start_paused = true)]
async fn named_servers_match_by_normalized_name_and_unknown_names_are_listed() {
    let rig = rig(
        FakeMcp::default().with("my server", ready()),
        &["my server"],
    );
    rig.manager.wait_ready().await;
    let (output, _) = call(&rig, json!({"servers": ["my_server", "nope"]})).await;
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        "ready: false\nConnected (their tools are now available \u{2014} call them directly): my server\nUnknown (no MCP server with this name is configured): nope"
    );
    let (output, _) = call(&rig, json!({"servers": ["my server"]})).await;
    assert!(!output.is_error);
}

#[tokio::test(start_paused = true)]
async fn disabled_servers_are_not_ready() {
    let rig = rig(FakeMcp::default(), &[]);
    rig.manager.register_disabled(vec![config("off")]);
    let (output, _) = call(&rig, json!({"servers": ["off"]})).await;
    assert!(output.is_error);
    assert_eq!(
        text(&output),
        "ready: false\nDisabled (ask the user to enable via /mcp): off"
    );
    // 什么都没有要等的：ready。
    let (output, _) = call(&rig, json!({})).await;
    assert_eq!(text(&output), "ready: true");
}
