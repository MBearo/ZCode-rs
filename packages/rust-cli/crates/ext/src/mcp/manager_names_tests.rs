//! 名字太长的 MCP 工具不发给模型（ext.md §3.4，修复线 F67）：模型用的工具表与查找都没有它，`/mcp` 的工具数照旧。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use serde_json::json;

use super::*;
use crate::mcp::McpScope;
use crate::test_support::{FakeMcp, server, workspace};

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

/// `mcp__files__` 是 12 个字符：116 个字符的工具名正好 128，117 个就超了。
fn listing() -> Arc<crate::test_support::Handler> {
    server(json!({"tools": {}}), None, |method, _| match method {
        "tools/list" => Some(Ok(json!({"tools": [
            {"name": "read", "inputSchema": {"type": "object"}},
            {"name": "a".repeat(116), "inputSchema": {"type": "object"}},
            {"name": "b".repeat(117), "inputSchema": {"type": "object"}},
        ]}))),
        _ => None,
    })
}

fn manager(fake: Arc<FakeMcp>, scope: &TaskScope) -> McpManager {
    McpManager::new(
        fake,
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    )
}

#[tokio::test]
async fn tools_with_names_over_128_characters_are_not_sent_to_the_model() {
    let fake = Arc::new(FakeMcp::default().with("files", listing()));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake, &scope);
    manager.start(workspace(), vec![stdio("files")]);
    manager.wait_ready().await;
    let exact = format!("mcp__files__{}", "a".repeat(116));
    let long = format!("mcp__files__{}", "b".repeat(117));
    assert_eq!(exact.len(), 128);
    let names: Vec<String> = manager
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(names, [exact.clone(), "mcp__files__read".to_owned()]);
    assert!(manager.find(&exact).is_some());
    assert!(manager.find(&long).is_none());
    // `/mcp` 的工具数仍是服务器列出的全部工具。
    assert_eq!(manager.server_infos()[0].tools, 3);
}

#[tokio::test]
async fn a_long_name_is_logged_once() {
    let fake = Arc::new(FakeMcp::default().with("files", listing()));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake.clone(), &scope);
    manager.start(workspace(), vec![stdio("files")]);
    manager.wait_ready().await;
    let mut changes = manager.0.changed.subscribe();
    // 服务器重新列出同样的工具：名字已经记过，不再记。
    fake.push(
        "files",
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
    );
    changes.changed().await.unwrap();
    let noted = manager.0.noted_names.lock().unwrap().clone();
    assert_eq!(
        noted.into_iter().collect::<Vec<_>>(),
        [format!("mcp__files__{}", "b".repeat(117))]
    );
}
