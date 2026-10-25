//! 本地 MCP 传输：stdio 的分帧、环境、退出与关闭；streamable HTTP 与 SSE 的消息往返（ext.md §3.2）。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::EnvironmentId;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

fn transport(egress: Option<Arc<Egress>>) -> (LocalMcpTransport, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let env = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("ZCODE_API_KEY".to_owned(), "secret".to_owned()),
    ];
    (
        LocalMcpTransport::new(dir.path().join("ledger"), env.into(), egress),
        dir,
    )
}

fn stdio(command: &str, args: &[&str], env: &[(&str, &str)]) -> McpConnect {
    McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            cwd: None,
        },
        env: Default::default(),
    }
}

async fn next(events: &mut BoxStream<'static, McpTransportEvent>) -> McpTransportEvent {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("an event in time")
        .expect("the stream is open")
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_round_trips_lines_and_closes() {
    let (transport, dir) = transport(None);
    let id = transport
        .connect(
            &workspace(dir.path().to_str().unwrap()),
            stdio("cat", &[], &[]),
        )
        .await
        .unwrap();
    let mut events = transport.events(&id);
    let message = json!({"jsonrpc": "2.0", "id": 0, "method": "ping"});
    transport.send(&id, message.clone()).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Message { message }
    );
    transport.close(&id).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Closed {
            reason: None,
            session_expired: false
        }
    );
    assert!(transport.send(&id, json!({})).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_projects_the_environment_and_reports_exit() {
    let (transport, dir) = transport(None);
    let script = r#"printf '{"key":"%s","api":"%s"}\nnot json\n' "$TOKEN" "$ZCODE_API_KEY""#;
    let id = transport
        .connect(
            &workspace(dir.path().to_str().unwrap()),
            stdio("/bin/sh", &["-c", script], &[("TOKEN", "abc")]),
        )
        .await
        .unwrap();
    let mut events = transport.events(&id);
    // 配置的变量传进去；凭据类变量不在白名单里；不是 JSON 的行丢弃。
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Message {
            message: json!({"key": "abc", "api": ""})
        }
    );
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Closed {
            reason: Some("MCP server \"s\" process exited".to_owned()),
            session_expired: false
        }
    );
}

/// Node 配置的 `cwd`（ext.md §3.1、§3.2）：相对路径相对工作区根，绝对路径原样，没有时是工作区根。
#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "测试在临时目录里建子目录、取真实路径"
)]
async fn stdio_starts_in_the_configured_directory() {
    let (transport, dir) = transport(None);
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("tools/mcp")).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere = std::fs::canonicalize(elsewhere.path()).unwrap();
    let script = r#"printf '{"cwd":"%s"}\n' "$(pwd -P)""#;
    let cases = [
        (None, root.clone()),
        (Some("tools/mcp".to_owned()), root.join("tools/mcp")),
        (
            Some(elsewhere.to_string_lossy().into_owned()),
            elsewhere.clone(),
        ),
    ];
    for (cwd, expected) in cases {
        let mut request = stdio("/bin/sh", &["-c", script], &[]);
        if let McpTransportConfig::Stdio { cwd: slot, .. } = &mut request.transport {
            slot.clone_from(&cwd);
        }
        let id = transport
            .connect(&workspace(root.to_str().unwrap()), request)
            .await
            .unwrap();
        let mut events = transport.events(&id);
        assert_eq!(
            next(&mut events).await,
            McpTransportEvent::Message {
                message: json!({"cwd": expected.to_string_lossy()})
            },
            "{cwd:?}"
        );
    }
}

#[tokio::test]
async fn a_missing_command_fails_to_connect() {
    let (transport, dir) = transport(None);
    let error = transport
        .connect(
            &workspace(dir.path().to_str().unwrap()),
            stdio("/nonexistent/mcp-server", &[], &[]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, McpError::Connect { message } if message.starts_with("MCP server \"s\" failed to start"))
    );
}

/// 一个 HTTP 请求：方法、路径、header（小写）、正文。
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

async fn read_request(socket: &mut TcpStream) -> Option<Request> {
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        seen.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&seen).into_owned();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
        let headers: BTreeMap<String, String> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if body.len() >= length {
            return Some(Request {
                method,
                path,
                headers,
                body: body.to_owned(),
            });
        }
    }
}

fn reply(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn egress() -> Arc<Egress> {
    Arc::new(
        Egress::new(rust_cli_net::NetworkPolicy::default())
            .await
            .unwrap(),
    )
}

#[expect(clippy::disallowed_methods, reason = "测试搭本地 HTTP 夹具")]
async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    (listener, base)
}

#[tokio::test]
async fn streamable_http_posts_json_and_keeps_the_session() {
    let (listener, base) = listener().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let Some(request) = read_request(&mut socket).await else {
                continue;
            };
            let answer = if request.method == "GET" {
                reply("405 Method Not Allowed", "", "")
            } else if request.body.contains("\"id\"") {
                let id = serde_json::from_str::<Value>(&request.body).unwrap()["id"].clone();
                let body = json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}).to_string();
                reply(
                    "200 OK",
                    "content-type: application/json\r\nmcp-session-id: sess-9\r\n",
                    &body,
                )
            } else {
                reply("202 Accepted", "", "")
            };
            log.lock().unwrap().push((
                request.method,
                request.path,
                request.headers.get("mcp-session-id").cloned(),
            ));
            socket.write_all(answer.as_bytes()).await.unwrap();
        }
    });
    let (transport, dir) = transport(Some(egress().await));
    let connect = McpConnect {
        server: "web".to_owned(),
        transport: McpTransportConfig::Http {
            url: format!("{base}/mcp"),
            headers: BTreeMap::from([("x-token".to_owned(), "t".to_owned())]),
        },
        env: Default::default(),
    };
    let id = transport
        .connect(&workspace(dir.path().to_str().unwrap()), connect)
        .await
        .unwrap();
    let mut events = transport.events(&id);
    transport
        .send(
            &id,
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"}),
        )
        .await
        .unwrap();
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Message {
            message: json!({"jsonrpc": "2.0", "id": 0, "result": {"ok": true}})
        }
    );
    transport
        .send(
            &id,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await
        .unwrap();
    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests,
        [
            ("POST".to_owned(), "/mcp".to_owned(), None),
            (
                "POST".to_owned(),
                "/mcp".to_owned(),
                Some("sess-9".to_owned())
            ),
            (
                "GET".to_owned(),
                "/mcp".to_owned(),
                Some("sess-9".to_owned())
            ),
        ]
    );
    server.abort();
}

#[tokio::test]
async fn sse_waits_for_the_endpoint_and_reads_messages_from_the_stream() {
    let (listener, base) = listener().await;
    let (push, pushed) = mpsc::unbounded_channel::<String>();
    let server = tokio::spawn(async move {
        let mut pushed = Some(pushed);
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let Some(request) = read_request(&mut socket).await else {
                continue;
            };
            if request.method == "GET" {
                // 事件流一直开着：放到自己的任务里，接着接受 POST。
                let mut pushed = pushed.take().unwrap();
                tokio::spawn(async move {
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\nevent: endpoint\r\ndata: /messages?session=1\r\n\r\n";
                    socket.write_all(head.as_bytes()).await.unwrap();
                    while let Some(event) = pushed.recv().await {
                        socket.write_all(event.as_bytes()).await.unwrap();
                    }
                });
                continue;
            }
            assert_eq!(request.path, "/messages?session=1");
            let id = serde_json::from_str::<Value>(&request.body).unwrap()["id"].clone();
            let message = json!({"jsonrpc": "2.0", "id": id, "result": {}});
            push.send(format!("event: message\ndata: {message}\n\n"))
                .unwrap();
            socket
                .write_all(reply("202 Accepted", "", "").as_bytes())
                .await
                .unwrap();
        }
    });
    let (transport, dir) = transport(Some(egress().await));
    let connect = McpConnect {
        server: "legacy".to_owned(),
        transport: McpTransportConfig::Sse {
            url: format!("{base}/sse"),
            headers: BTreeMap::new(),
        },
        env: Default::default(),
    };
    let id = transport
        .connect(&workspace(dir.path().to_str().unwrap()), connect)
        .await
        .unwrap();
    let mut events = transport.events(&id);
    transport
        .send(&id, json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}))
        .await
        .unwrap();
    assert_eq!(
        next(&mut events).await,
        McpTransportEvent::Message {
            message: json!({"jsonrpc": "2.0", "id": 3, "result": {}})
        }
    );
    transport.close(&id).await.unwrap();
    assert!(matches!(
        next(&mut events).await,
        McpTransportEvent::Closed { reason: None, .. }
    ));
    server.abort();
}

/// 缺陷 R2-5：SSE 的 `endpoint` 事件原来可以指向别的 origin，配置的 header（常有 Authorization）会跟着 POST
/// 过去。按 MCP SDK（`SSEClientTransport`）：join 后 origin 不同时连接失败。
#[tokio::test]
async fn an_sse_endpoint_on_another_origin_fails_to_connect() {
    let (listener, base) = listener().await;
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            if read_request(&mut socket).await.is_none() {
                continue;
            }
            tokio::spawn(async move {
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\nevent: endpoint\r\ndata: https://elsewhere.example.com/messages\r\n\r\n";
                socket.write_all(head.as_bytes()).await.unwrap();
                tokio::time::sleep(Duration::from_secs(10)).await;
            });
        }
    });
    let (transport, dir) = transport(Some(egress().await));
    let connect = McpConnect {
        server: "legacy".to_owned(),
        transport: McpTransportConfig::Sse {
            url: format!("{base}/sse"),
            headers: BTreeMap::from([("authorization".to_owned(), "Bearer t".to_owned())]),
        },
        env: Default::default(),
    };
    let error = transport
        .connect(&workspace(dir.path().to_str().unwrap()), connect)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        McpError::Connect {
            message:
                "Endpoint origin does not match connection origin: https://elsewhere.example.com"
                    .to_owned()
        }
        .to_string()
    );
    server.abort();
}
