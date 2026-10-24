//! ws 来源的 monitor（tools-more.md §19.7）：文本帧进输出文件、二进制帧写成占位行、关闭写进生命周期；连不上时写出错与
//! 1006 关闭；被停掉时不写生命周期、不记通知。本地的 WebSocket 服务。
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, OutputStream};
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundKind, BackgroundTask, CwdScope, MonitorSocket,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_net::{Egress, NetworkPolicy};

use crate::shell::tests::{context, fixture, request};

/// 回 101，写两帧（文本、二进制）再写 close（`hold` 时不关，等客户端断开）。
#[expect(clippy::disallowed_methods, reason = "测试的本地 WebSocket 服务")]
fn serve(hold: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let key = head
            .lines()
            .find_map(|line| line.strip_prefix("sec-websocket-key: "))
            .unwrap();
        let accept = accept(key);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(&[0x81, 5]).unwrap();
        stream.write_all(b"hello").unwrap();
        stream.write_all(&[0x82, 3, 1, 2, 3]).unwrap();
        if hold {
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest);
            return;
        }
        stream.write_all(&[0x88, 2, 0x03, 0xe8]).unwrap();
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });
    format!("ws://127.0.0.1:{port}/feed")
}

/// 测试里另算一次 accept（net 的实现是 crate 内部的）：经 net 的握手校验通过即说明一致，这里直接借用服务端的写法。
fn accept(key: &str) -> String {
    rust_cli_net::websocket::accept_for_tests(key)
}

fn socket_request(url: &str, root: &std::path::Path) -> rust_cli_kernel_api::env::StartCommand {
    let mut start = request(url, root, CwdScope::Pinned);
    start.timeout_ms = None;
    start.background = Some(BackgroundCommand {
        description: "feed".to_owned(),
        tool_use: None,
        kind: BackgroundKind::Monitor,
    });
    start.socket = Some(MonitorSocket {
        url: url.to_owned(),
        protocols: Vec::new(),
    });
    start
}

async fn web() -> Option<std::sync::Arc<Egress>> {
    Some(std::sync::Arc::new(
        Egress::new(NetworkPolicy::default()).await.unwrap(),
    ))
}

async fn drain(
    fixture: &crate::shell::tests::Fixture,
    operation: &str,
) -> (String, String, Option<CommandEvent>) {
    let mut events = fixture
        .shells
        .watch(&OperationId::new(operation), 0)
        .unwrap();
    let (mut stdout, mut stderr, mut finished) = (Vec::new(), Vec::new(), None);
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output {
                stream: OutputStream::Stdout,
                data,
                ..
            } => stdout.extend(data),
            CommandEvent::Output {
                stream: OutputStream::Stderr,
                data,
                ..
            } => stderr.extend(data),
            finish @ CommandEvent::Finished { .. } => finished = Some(finish),
            _ => {}
        }
    }
    (
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
        finished,
    )
}

#[tokio::test]
async fn frames_go_to_the_output_and_the_close_to_the_lifecycle() {
    let fixture = fixture(Vec::new());
    let url = serve(false);
    fixture
        .shells
        .start_with(
            context("op_ws"),
            socket_request(&url, &fixture.root),
            web().await,
        )
        .await
        .unwrap();
    let (stdout, stderr, _) = drain(&fixture, "op_ws").await;
    assert_eq!(stdout, "hello\n[binary frame, 3 bytes]\n");
    assert_eq!(stderr, "{\"type\":\"closed\",\"code\":1000}\n");
    let notice = fixture
        .shells
        .notices(&SessionId::new("s1"), 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.kind, BackgroundKind::Monitor);
}

#[expect(clippy::disallowed_methods, reason = "测试先占一个端口再放掉")]
#[tokio::test]
async fn a_refused_connection_reports_an_error_then_1006() {
    let fixture = fixture(Vec::new());
    // 先占一个端口再放掉：连上去被拒绝。
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("ws://127.0.0.1:{port}/feed");
    fixture
        .shells
        .start_with(
            context("op_ws"),
            socket_request(&url, &fixture.root),
            web().await,
        )
        .await
        .unwrap();
    let (stdout, stderr, _) = drain(&fixture, "op_ws").await;
    assert_eq!(stdout, "");
    let lines: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{stderr}");
    assert_eq!(lines[0]["type"], "error");
    assert!(
        lines[0]["message"]
            .as_str()
            .unwrap()
            .starts_with(&format!("WebSocket connection to '{url}' failed: "))
    );
    assert_eq!(lines[1]["type"], "closed");
    assert_eq!(lines[1]["code"], 1006);
}

#[tokio::test]
async fn a_stopped_socket_writes_no_lifecycle_and_no_notice() {
    let fixture = fixture(Vec::new());
    let url = serve(true);
    let start = socket_request(&url, &fixture.root);
    let task = start.task.clone();
    fixture
        .shells
        .start_with(context("op_ws"), start, web().await)
        .await
        .unwrap();
    // 等第一帧写出来，再停。
    let mut events = fixture.shells.watch(&OperationId::new("op_ws"), 0).unwrap();
    while let Some(event) = events.next().await {
        if matches!(event.unwrap(), CommandEvent::Output { .. }) {
            break;
        }
    }
    let stop = fixture
        .shells
        .stop(
            &context("op_stop"),
            &BackgroundTask {
                session: SessionId::new("s1"),
                task,
            },
        )
        .unwrap();
    assert!(matches!(
        stop,
        rust_cli_kernel_api::env_shell::BackgroundStop::Stopped { .. }
    ));
    let (_, stderr, finished) = drain(&fixture, "op_ws").await;
    assert_eq!(stderr, "");
    assert!(matches!(finished, Some(CommandEvent::Finished { .. })));
    let pending = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.shells.notices(&SessionId::new("s1"), 0).next(),
    )
    .await;
    assert!(pending.is_err());
}
