//! `/mcp reconnect|enable|disable`（tui-manage.md §6.1）：重连换新连接，停用停掉任务，启用重新连上。
use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::manage::{McpAction, McpServerState};
use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::json;

use super::super::McpSettings;
use super::*;
use crate::mcp::McpScope;
use crate::test_support::{FakeMcp, server, workspace};

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
        machine: None,
    }
}

fn fake() -> Arc<FakeMcp> {
    let tools = || {
        server(json!({"tools": {}}), None, |method, _| match method {
            "tools/list" => Some(Ok(json!({"tools": [
                {"name": "read", "inputSchema": {"type": "object"}}
            ]}))),
            _ => None,
        })
    };
    Arc::new(
        FakeMcp::default()
            .with("files", tools())
            .with("off", tools()),
    )
}

fn handshakes(fake: &FakeMcp, name: &str) -> usize {
    fake.received()
        .iter()
        .filter(|(server, message)| server == name && message["method"] == "initialize")
        .count()
}

fn names(manager: &McpManager) -> Vec<String> {
    manager
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

#[tokio::test]
async fn reconnect_disable_and_enable_drive_the_connection() {
    let fake = fake();
    let scope = TaskScope::root("test", |_| {});
    let manager = McpManager::new(
        fake.clone(),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    );
    manager.start(workspace(), vec![config("files")]);
    manager.register_disabled(vec![config("off"), config("files")]);
    manager.wait_ready().await;
    assert_eq!(handshakes(&fake, "files"), 1);
    let states = |infos: &[McpServerInfo]| {
        infos
            .iter()
            .map(|info| (info.name.clone(), info.state))
            .collect::<Vec<_>>()
    };
    // 启动时停用的登记为 disabled，不连接；已启动的不被它覆盖。
    assert_eq!(
        states(&manager.server_infos()),
        [
            ("files".to_owned(), McpServerState::Connected),
            ("off".to_owned(), McpServerState::Disabled)
        ]
    );
    assert_eq!(handshakes(&fake, "off"), 0);
    // 重连：新的握手，结果是已连接；停用的跳过。
    let infos = manager
        .control(
            McpAction::Reconnect,
            &["files".to_owned(), "off".to_owned()],
        )
        .await;
    assert_eq!(
        states(&infos),
        [
            ("files".to_owned(), McpServerState::Connected),
            ("off".to_owned(), McpServerState::Disabled)
        ]
    );
    assert_eq!(handshakes(&fake, "files"), 2);
    assert_eq!(names(&manager), ["mcp__files__read"]);
    // 停用：工具消失；旧连接再关闭也不改状态（代号不符）。
    let infos = manager
        .control(McpAction::Disable, &["files".to_owned()])
        .await;
    assert_eq!(
        states(&infos),
        [("files".to_owned(), McpServerState::Disabled)]
    );
    assert!(names(&manager).is_empty());
    fake.drop_server("files", false);
    tokio::task::yield_now().await;
    assert_eq!(manager.server_infos()[0].state, McpServerState::Disabled);
    // 启用：重新连上（包括启动时停用的）。
    let infos = manager
        .control(McpAction::Enable, &["files".to_owned(), "off".to_owned()])
        .await;
    assert_eq!(
        states(&infos),
        [
            ("files".to_owned(), McpServerState::Connected),
            ("off".to_owned(), McpServerState::Connected)
        ]
    );
    assert_eq!(names(&manager), ["mcp__files__read", "mcp__off__read"]);
    // 已启用的再启用：不动。
    manager
        .control(McpAction::Enable, &["files".to_owned()])
        .await;
    assert_eq!(handshakes(&fake, "files"), 3);
}
