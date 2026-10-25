//! 连上过的 stdio 服务器断开（ext.md §3.3.3）：`-p` 留着工具、下次调用再连；TUI 记为失败；资源工具留下。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::MemoryEnvironment;
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::mcp::McpScope;
use crate::test_support::{FakeMcp, facts, server, tool_context, workspace};

fn stdio(name: &str) -> McpServer {
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
        machine: None,
    }
}

fn files() -> Arc<crate::test_support::Handler> {
    server(
        json!({"tools": {}, "resources": {}}),
        None,
        |method, params| match method {
            "tools/list" => Some(Ok(json!({"tools": [
                {"name": "read", "inputSchema": {"type": "object"}}
            ]}))),
            "tools/call" => Some(Ok(json!({"content": [
                {"type": "text", "text": format!("read {}", params["arguments"]["path"].as_str().unwrap_or_default())}
            ]}))),
            _ => None,
        },
    )
}

fn names(manager: &McpManager) -> Vec<String> {
    manager
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

fn initializes(fake: &FakeMcp) -> usize {
    fake.received()
        .iter()
        .filter(|(_, message)| message["method"] == "initialize")
        .count()
}

async fn started(print: bool) -> (Arc<FakeMcp>, McpManager, TaskScope) {
    let fake = Arc::new(FakeMcp::default().with("files", files()));
    let scope = TaskScope::root("test", |_| {});
    let manager = McpManager::new(
        fake.clone(),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    );
    manager.set_print_session(print);
    manager.start(workspace(), vec![stdio("files")]);
    manager.wait_ready().await;
    (fake, manager, scope)
}

const ALL: [&str; 4] = [
    "ListMcpResourcesTool",
    "ReadMcpResourceDirTool",
    "ReadMcpResourceTool",
    "mcp__files__read",
];

#[tokio::test]
async fn a_print_session_keeps_a_closed_stdio_server_and_reconnects_on_the_next_call() {
    let (fake, manager, _scope) = started(true).await;
    let mut changes = manager.0.changed.subscribe();
    fake.drop_server("files", false);
    while manager.0.client("files").is_some() {
        changes.changed().await.unwrap();
    }
    // 工具与状态不变：模型看不到断开。
    assert_eq!(manager.status()[0].1, McpStatus::Connected);
    assert_eq!(names(&manager), ALL);
    let read = manager.find("mcp__files__read").unwrap();
    let context = tool_context(
        Arc::new(MemoryEnvironment::new(facts())),
        PermissionMode::Default,
    );
    let (progress, _ignored) = mpsc::channel(4);
    let output = read
        .call(json!({"path": "a.txt"}), context, progress)
        .await
        .unwrap();
    assert!(
        matches!(&output.content[..], [ToolResultContent::Text { text }] if text == "read a.txt")
    );
    assert_eq!(initializes(&fake), 2);
}

#[tokio::test]
async fn in_the_terminal_a_closed_stdio_server_fails_but_the_resource_tools_stay() {
    let (fake, manager, _scope) = started(false).await;
    let mut changes = manager.0.changed.subscribe();
    fake.drop_server("files", false);
    while !matches!(manager.status()[0].1, McpStatus::Failed { .. }) {
        changes.changed().await.unwrap();
    }
    assert_eq!(names(&manager), ALL[..3]);
    assert_eq!(initializes(&fake), 1);
}
