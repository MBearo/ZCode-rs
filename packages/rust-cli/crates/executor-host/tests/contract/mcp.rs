//! MCP 的管道（R5，remote-exec.md §22.4、§22.6）：本机的 `LocalMcpTransport` 与“env-remote + 内存 loopback + exec-server
//! + 同一个 `LocalMcpTransport`”结果相同——消息往返、关闭、服务器退出、起不来时的原文。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_kernel_api::mcp::{McpConnect, McpTransportConfig, McpTransportEvent};
use serde_json::json;

use crate::contract;
use crate::support::{Kind, machine_with_mcp, workspace};

async fn setup(kind: Kind) -> (crate::support::Machine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let env = vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
    let mcp = Arc::new(LocalMcpTransport::new(
        dir.path().join("ledger"),
        env.into(),
        None,
    ));
    let local = LocalEnvironment::new(None);
    (machine_with_mcp(kind, local, mcp).await, dir)
}

fn stdio(command: &str, args: &[&str]) -> McpConnect {
    McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: Default::default(),
            cwd: None,
        },
        env: Default::default(),
    }
}

async fn next(events: &mut BoxStream<'static, McpTransportEvent>) -> Option<McpTransportEvent> {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap_or_else(|_| panic!("no event in time"))
}

async fn messages_round_trip_and_close(kind: Kind) {
    let (machine, dir) = setup(kind).await;
    let mcp = machine.mcp();
    let id = mcp
        .connect(&workspace(dir.path()), stdio("cat", &[]))
        .await
        .unwrap();
    let mut events = mcp.events(&id);
    let message = json!({"jsonrpc": "2.0", "id": 0, "method": "ping"});
    mcp.send(&id, message.clone()).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Message { message })
    );
    mcp.close(&id).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Closed {
            reason: None,
            session_expired: false
        })
    );
    assert_eq!(next(&mut events).await, None);
    assert!(mcp.send(&id, json!({})).await.is_err());
}

async fn a_server_that_exits_is_reported(kind: Kind) {
    let (machine, dir) = setup(kind).await;
    let mcp = machine.mcp();
    let script = r#"printf '{"hello":"world"}\n'"#;
    let id = mcp
        .connect(&workspace(dir.path()), stdio("/bin/sh", &["-c", script]))
        .await
        .unwrap();
    let mut events = mcp.events(&id);
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Message {
            message: json!({"hello": "world"})
        })
    );
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Closed {
            reason: Some("MCP server \"s\" process exited".to_owned()),
            session_expired: false
        })
    );
}

async fn a_server_that_cannot_start_keeps_its_message(kind: Kind) {
    let (machine, dir) = setup(kind).await;
    let error = machine
        .mcp()
        .connect(&workspace(dir.path()), stdio("/no/such/mcp-server", &[]))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("MCP server \"s\" failed to start: "),
        "{error}"
    );
}

contract!(
    messages_round_trip_and_close,
    a_server_that_exits_is_reported,
    a_server_that_cannot_start_keeps_its_message,
);
