//! 通道有界（F11，审查 R1-18；docs/specs/rust-cli/ext.md §3.2“通道与溢出”）：服务器的输出在读者不读时背压到
//! 服务器、一条不丢；服务器不读输入时发送立即报错，关闭照常完成。需要 `/bin/sh`。
#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::EnvironmentId;
use serde_json::json;

use super::*;

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

fn transport() -> (LocalMcpTransport, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let env = vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
    (
        LocalMcpTransport::new(dir.path().join("ledger"), env.into(), None),
        dir,
    )
}

fn shell(script: &str) -> McpConnect {
    McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            env: BTreeMap::new(),
            cwd: None,
        },
        env: Default::default(),
    }
}

#[expect(clippy::disallowed_methods, reason = "测试看服务器有没有写完")]
fn exists(path: &std::path::Path) -> bool {
    path.exists()
}

const LINES: usize = 20_000;

/// 读者不读时，服务器写满管道后停在输出上（没写完）；之后读者读走全部消息、按顺序，再收到关闭。旧代码是无界通道：
/// 连接任务把两万条全读进内存，服务器早就写完退出。
#[tokio::test]
async fn server_output_waits_for_the_reader_and_none_is_lost() {
    let (transport, dir) = transport();
    let done = dir.path().join("done");
    let script = format!(
        r#"i=0; while [ $i -lt {LINES} ]; do echo "{{\"jsonrpc\":\"2.0\",\"method\":\"n\",\"params\":{{\"i\":$i,\"pad\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}}}}"; i=$((i+1)); done; : > '{}'"#,
        done.display()
    );
    let id = transport
        .connect(&workspace(dir.path().to_str().unwrap()), shell(&script))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!exists(&done), "the server should wait for the reader");
    let mut events = transport.events(&id);
    for expected in 0..LINES {
        let event = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .unwrap()
            .unwrap();
        let McpTransportEvent::Message { message } = event else {
            panic!("expected message {expected}, got {event:?}");
        };
        assert_eq!(message["params"]["i"], json!(expected));
    }
    let closed = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(McpTransportEvent::Closed { .. })));
    assert!(exists(&done));
}

/// 服务器不读输入：发件箱满了以后发送立即报错（不卡住调用方），关闭不经发件箱、照常完成。旧代码的发件箱无界，
/// 发送永远成功、消息无限堆积，关闭排在堆积的消息后面、等一个不读输入的服务器。
#[tokio::test]
async fn a_server_that_reads_nothing_fails_sends_and_still_closes() {
    let (transport, dir) = transport();
    let id = transport
        .connect(&workspace(dir.path().to_str().unwrap()), shell("sleep 30"))
        .await
        .unwrap();
    let message = json!({"jsonrpc": "2.0", "method": "n", "params": {"pad": "x".repeat(1000)}});
    let mut failed = None;
    for sent in 0..10_000 {
        if let Err(error) = transport.send(&id, message.clone()).await {
            failed = Some((sent, error));
            break;
        }
        tokio::task::yield_now().await;
    }
    let (sent, error) = failed.expect("sending to a server that reads nothing fails");
    assert!(sent >= OUTBOX_CAPACITY, "{sent}");
    assert!(
        error.to_string().contains("is not reading its input"),
        "{error}"
    );
    tokio::time::timeout(Duration::from_secs(10), transport.close(&id))
        .await
        .expect("close does not wait for the server to read")
        .unwrap();
}
