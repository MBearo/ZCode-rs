//! MCP 的管道在执行器一侧（remote-exec.md §22.4、§22.5）：连接、发送、事件、关闭经注入的传输；别的客户端实例拿不到；
//! 没报告能力位时不执行；断线后按 `seq` 补发事件、服务器不被关掉；断开太久的实例的连接被关掉。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::Workspace;
use rust_cli_exec_wire::dto::mcp::{McpEvent, McpTransport as WireTransport};
use rust_cli_exec_wire::envelope::{
    Ack, EventHeader, ReplyResult, RequestHeader, StreamAck, StreamItem, WireError,
};
use rust_cli_exec_wire::frame::Kind;
use rust_cli_exec_wire::handshake::{Capabilities, Hello};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportConfig, McpTransportEvent,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::MemoryLedger;
use crate::test_support::{Client, TestMachine, config, connect, hello, server_with_mcp};

/// 一条连接的两头：发事件的一头，与还没被取走的事件流。
type Link = (
    mpsc::UnboundedSender<McpTransportEvent>,
    Option<mpsc::UnboundedReceiver<McpTransportEvent>>,
);

/// MCP 传输的替身：记下连接的配置、发出的消息与关闭；每条请求原样回一条结果（像一个服务器）。
#[derive(Default)]
struct FakeMcp {
    connects: Mutex<Vec<(WorkspaceRef, McpConnect)>>,
    sent: Mutex<Vec<Value>>,
    closed: Mutex<Vec<String>>,
    links: Mutex<Vec<Link>>,
}

impl FakeMcp {
    /// 服务器进程自己退出（不是被关掉）：送出 Closed，之后流结束。
    fn exit(&self, index: usize) {
        let mut links = self.links.lock().unwrap();
        let _ = links[index].0.send(McpTransportEvent::Closed {
            reason: Some("MCP server \"browser-a\" process exited".to_owned()),
            session_expired: false,
        });
        links[index].0 = mpsc::unbounded_channel().0;
    }

    fn index(connection: &McpConnectionId) -> usize {
        connection
            .as_str()
            .trim_start_matches("mcp-")
            .parse()
            .unwrap()
    }
}

#[async_trait]
impl McpTransport for FakeMcp {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        if request.server == "broken" {
            return Err(McpError::Connect {
                message: "MCP server \"broken\" failed to start: no such file".to_owned(),
            });
        }
        self.connects
            .lock()
            .unwrap()
            .push((workspace.clone(), request));
        let (tx, rx) = mpsc::unbounded_channel();
        let mut links = self.links.lock().unwrap();
        links.push((tx, Some(rx)));
        Ok(McpConnectionId::new(format!("mcp-{}", links.len() - 1)))
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        self.sent.lock().unwrap().push(message.clone());
        let links = self.links.lock().unwrap();
        let (tx, _) = &links[Self::index(connection)];
        let reply =
            json!({"jsonrpc": "2.0", "id": message["id"], "result": {"echo": message["method"]}});
        tx.send(McpTransportEvent::Message { message: reply })
            .map_err(|_| McpError::Closed {
                message: "the server process has exited".to_owned(),
            })
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        let taken = self.links.lock().unwrap()[Self::index(connection)].1.take();
        match taken {
            Some(mut rx) => futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed(),
            None => futures_util::stream::empty().boxed(),
        }
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        self.closed
            .lock()
            .unwrap()
            .push(connection.as_str().to_owned());
        let mut links = self.links.lock().unwrap();
        let (tx, _) = &links[Self::index(connection)];
        let _ = tx.send(McpTransportEvent::Closed {
            reason: None,
            session_expired: false,
        });
        // 换掉发送端：之后流结束（同 LocalMcpTransport 送出 Closed 后结束）。
        links[Self::index(connection)].0 = mpsc::unbounded_channel().0;
        Ok(())
    }
}

fn workspace() -> Workspace {
    Workspace {
        environment: "dev-a".to_owned(),
        root: "/srv/proj".to_owned(),
    }
}

fn connect_request(server: &str) -> Request {
    Request::McpConnect {
        workspace: workspace(),
        server: server.to_owned(),
        transport: WireTransport::Stdio {
            command: "server".to_owned(),
            args: vec!["--token".to_owned(), "${TOKEN}".to_owned()],
            env: Default::default(),
            cwd: None,
        },
    }
}

async fn mcp_server(fake: Arc<FakeMcp>, settings: crate::ServerConfig) -> crate::ExecServer {
    server_with_mcp(
        Arc::new(TestMachine::default()),
        fake,
        Arc::new(MemoryLedger::default()),
        settings,
    )
    .await
}

fn with_mcp() -> crate::ServerConfig {
    let mut settings = config();
    settings.capabilities = Capabilities::ports_and_mcp();
    settings
}

async fn reply(client: &mut Client) -> ReplyResult {
    client.reply().await.0.result
}

async fn event(client: &mut Client) -> EventHeader {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), client.frame())
            .await
            .unwrap()
            .unwrap();
        if frame.kind == Kind::Event {
            return frame.header().unwrap();
        }
    }
}

async fn connected(client: &mut Client) -> String {
    client
        .request(1, connect_request("browser-a"), &Body::empty())
        .await;
    match reply(client).await {
        ReplyResult::Ok(Reply::McpConnect { connection }) => connection,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_server_connects_takes_messages_and_streams_its_events() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    let connection = connected(&mut client).await;
    // 连接 ID 带执行器实例（重启后不重名）；配置原样交给传输（`${TOKEN}` 由执行器的传输展开，这里不动）。
    assert_eq!(connection, format!("{}-mcp-0", server.instance()));
    let (ws, request) = fake.connects.lock().unwrap()[0].clone();
    assert_eq!(
        (ws.environment.as_str(), ws.root.as_str()),
        ("dev-a", "/srv/proj")
    );
    assert_eq!(
        request.transport,
        McpTransportConfig::Stdio {
            command: "server".to_owned(),
            args: vec!["--token".to_owned(), "${TOKEN}".to_owned()],
            env: Default::default(),
            cwd: None,
        }
    );
    let events = Request::McpEvents {
        connection: connection.clone(),
    };
    client.request(2, events, &Body::empty()).await;
    let send = Request::McpSend {
        connection: connection.clone(),
        message: json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"}),
    };
    client.request(3, send, &Body::empty()).await;
    let mut got_reply = false;
    let mut got_event = None;
    while !(got_reply && got_event.is_some()) {
        let frame = client.frame().await.unwrap();
        match frame.kind {
            Kind::Reply => {
                let header: rust_cli_exec_wire::envelope::ReplyHeader = frame.header().unwrap();
                assert_eq!(header.call, 3);
                assert_eq!(header.result, ReplyResult::Ok(Reply::McpSend));
                got_reply = true;
            }
            Kind::Event => got_event = Some(frame.header::<EventHeader>().unwrap()),
            _ => {}
        }
    }
    let first = got_event.unwrap();
    assert_eq!((first.call, first.seq), (2, 1));
    assert_eq!(
        first.item,
        StreamItem::Mcp {
            event: McpEvent::Message {
                message: json!({"jsonrpc": "2.0", "id": 0, "result": {"echo": "initialize"}})
            }
        }
    );
    client
        .request(4, Request::McpClose { connection }, &Body::empty())
        .await;
    assert_eq!(reply(&mut client).await, ReplyResult::Ok(Reply::McpClose));
    assert_eq!(fake.closed.lock().unwrap().as_slice(), ["mcp-0"]);
    let closed = event(&mut client).await;
    assert!(matches!(
        closed.item,
        StreamItem::Mcp {
            event: McpEvent::Closed { .. }
        }
    ));
    assert_eq!(event(&mut client).await.item, StreamItem::End);
}

#[tokio::test]
async fn a_failed_start_keeps_its_message_and_another_client_cannot_use_a_connection() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    client
        .request(1, connect_request("broken"), &Body::empty())
        .await;
    assert_eq!(
        reply(&mut client).await,
        ReplyResult::Err(WireError::Io {
            message: "MCP server \"broken\" failed to start: no such file".to_owned()
        })
    );
    client
        .request(2, connect_request("browser-a"), &Body::empty())
        .await;
    let ReplyResult::Ok(Reply::McpConnect { connection }) = reply(&mut client).await else {
        panic!("not connected");
    };
    // 另一个客户端实例（另一个 TUI）：拿到 ID 也用不了、关不掉。
    let (mut other, _other_task) = connect(&server);
    let stranger = Hello {
        client_instance: "client-2".to_owned(),
        ..hello(1)
    };
    other.send(Kind::Hello, &stranger, &Body::empty()).await;
    other.frame().await.unwrap();
    let send = Request::McpSend {
        connection: connection.clone(),
        message: json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    };
    other.request(1, send, &Body::empty()).await;
    assert!(matches!(
        reply(&mut other).await,
        ReplyResult::Err(WireError::NotFound { .. })
    ));
    other
        .request(2, Request::McpClose { connection }, &Body::empty())
        .await;
    assert_eq!(reply(&mut other).await, ReplyResult::Ok(Reply::McpClose));
    assert!(fake.sent.lock().unwrap().is_empty());
    assert!(fake.closed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_executor_that_does_not_report_mcp_does_not_run_it() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), config()).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    client
        .request(1, connect_request("browser-a"), &Body::empty())
        .await;
    assert!(matches!(
        reply(&mut client).await,
        ReplyResult::Err(WireError::Unsupported { .. })
    ));
    assert!(fake.connects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn events_are_replayed_after_a_reconnect_and_the_server_keeps_running() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut client, task) = connect(&server);
    client.handshake(1).await;
    let connection = connected(&mut client).await;
    let events = Request::McpEvents {
        connection: connection.clone(),
    };
    client.request(2, events.clone(), &Body::empty()).await;
    for (call, id) in [(3, 0), (4, 1)] {
        let send = Request::McpSend {
            connection: connection.clone(),
            message: json!({"jsonrpc": "2.0", "id": id, "method": "ping"}),
        };
        client.request(call, send, &Body::empty()).await;
    }
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let frame = client.frame().await.unwrap();
        if frame.kind == Kind::Event {
            seen.push(frame.header::<EventHeader>().unwrap().seq);
        }
    }
    assert_eq!(seen, [1, 2]);
    // 只 ack 了第一项就断线：服务器不被关掉。
    let ack = Ack {
        replies: vec![1, 3, 4],
        streams: vec![StreamAck { call: 2, seq: 1 }],
    };
    client.send(Kind::Ack, &ack, &Body::empty()).await;
    client.close().await;
    let _ = task.await;
    assert!(fake.closed.lock().unwrap().is_empty());
    // 重连：补发流，收到 seq 2（断线前没 ack 的），之后的消息照常从 seq 3 起。
    let (mut again, _task) = connect(&server);
    again.handshake(2).await;
    let resume = RequestHeader {
        call: 2,
        request: events,
        resume_after: Some(1),
    };
    again.send(Kind::Request, &resume, &Body::empty()).await;
    let replayed = event(&mut again).await;
    assert_eq!(replayed.seq, 2);
    assert_eq!(
        replayed.item,
        StreamItem::Mcp {
            event: McpEvent::Message {
                message: json!({"jsonrpc": "2.0", "id": 1, "result": {"echo": "ping"}})
            }
        }
    );
    let send = Request::McpSend {
        connection,
        message: json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    };
    again.request(5, send, &Body::empty()).await;
    assert_eq!(event(&mut again).await.seq, 3);
    assert_eq!(fake.connects.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_client_away_too_long_has_its_servers_closed() {
    let fake = Arc::new(FakeMcp::default());
    let mut settings = with_mcp();
    settings.abandon_after = Duration::from_millis(40);
    settings.sweep_every = Duration::from_millis(10);
    let server = mcp_server(fake.clone(), settings).await;
    let (mut client, task) = connect(&server);
    client.handshake(1).await;
    connected(&mut client).await;
    client.close().await;
    let _ = task.await;
    for _ in 0..200 {
        if !fake.closed.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(fake.closed.lock().unwrap().as_slice(), ["mcp-0"]);
}

/// 以 `instance` 握手的客户端。
async fn client_as(
    server: &crate::ExecServer,
    instance: &str,
) -> (Client, rust_cli_base::OwnedTask<()>) {
    let (mut client, task) = connect(server);
    let hello = Hello {
        client_instance: instance.to_owned(),
        ..hello(1)
    };
    client.send(Kind::Hello, &hello, &Body::empty()).await;
    client.frame().await.unwrap();
    (client, task)
}

fn connect_with(server: &str, args: &[&str]) -> Request {
    Request::McpConnect {
        workspace: workspace(),
        server: server.to_owned(),
        transport: WireTransport::Stdio {
            command: "server".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: [("TOKEN".to_owned(), "${TOKEN}".to_owned())].into(),
            cwd: None,
        },
    }
}

async fn connection_of(client: &mut Client, call: u64, request: Request) -> String {
    client.request(call, request, &Body::empty()).await;
    match reply(client).await {
        ReplyResult::Ok(Reply::McpConnect { connection }) => connection,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_same_config_from_a_new_client_reattaches_without_a_restart() {
    // 幂等（remote-exec.md §22.5）：上一个客户端（上一个进程、失败之前的连接）没连着了，同一个键接回同一个进程。
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut first, task) = client_as(&server, "tui-1").await;
    let connection = connection_of(&mut first, 1, connect_with("browser-a", &["--x"])).await;
    let events = Request::McpEvents {
        connection: connection.clone(),
    };
    first.request(2, events.clone(), &Body::empty()).await;
    first.close().await;
    let _ = task.await;
    let (mut second, _task) = client_as(&server, "tui-2").await;
    let again = connection_of(&mut second, 1, connect_with("browser-a", &["--x"])).await;
    assert_eq!(again, connection);
    assert_eq!(fake.connects.lock().unwrap().len(), 1, "not restarted");
    assert!(fake.closed.lock().unwrap().is_empty());
    // 新的订阅接着读：之后的消息到新的客户端。
    second.request(2, events, &Body::empty()).await;
    let send = Request::McpSend {
        connection,
        message: json!({"jsonrpc": "2.0", "id": 7, "method": "initialize"}),
    };
    second.request(3, send, &Body::empty()).await;
    let got = event(&mut second).await;
    assert_eq!(
        got.item,
        StreamItem::Mcp {
            event: McpEvent::Message {
                message: json!({"jsonrpc": "2.0", "id": 7, "result": {"echo": "initialize"}})
            }
        }
    );
}

#[tokio::test]
async fn a_changed_config_restarts_the_server() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut client, _task) = client_as(&server, "tui-1").await;
    let old = connection_of(&mut client, 1, connect_with("browser-a", &["--x"])).await;
    let new = connection_of(&mut client, 2, connect_with("browser-a", &["--y"])).await;
    assert_ne!(new, old);
    assert_eq!(fake.connects.lock().unwrap().len(), 2);
    assert_eq!(fake.closed.lock().unwrap().as_slice(), ["mcp-0"]);
    // 同一个配置再连：接回新的那个。
    let same = connection_of(&mut client, 3, connect_with("browser-a", &["--y"])).await;
    assert_eq!(same, new);
    assert_eq!(fake.connects.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_server_that_another_connected_client_uses_is_not_shared() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut first, _first_task) = client_as(&server, "tui-1").await;
    let mine = connection_of(&mut first, 1, connect_with("browser-a", &["--x"])).await;
    let (mut second, _second_task) = client_as(&server, "tui-2").await;
    let theirs = connection_of(&mut second, 1, connect_with("browser-a", &["--x"])).await;
    assert_ne!(mine, theirs);
    assert_eq!(fake.connects.lock().unwrap().len(), 2);
    assert!(fake.closed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_declared_set_closes_servers_that_are_no_longer_configured() {
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut first, task) = client_as(&server, "tui-1").await;
    let kept = connection_of(&mut first, 1, connect_with("browser-a", &["--x"])).await;
    connection_of(&mut first, 2, connect_with("docs", &[])).await;
    connection_of(&mut first, 3, connect_with("db", &["--old"])).await;
    first.close().await;
    let _ = task.await;
    // 新的进程（配置删了 docs、改了 db）连上机器后先声明期望集合。
    let (mut second, _task) = client_as(&server, "tui-2").await;
    let declared = |server: &str, args: &[&str]| {
        let Request::McpConnect {
            server, transport, ..
        } = connect_with(server, args)
        else {
            unreachable!()
        };
        rust_cli_exec_wire::dto::mcp::McpDeclared { server, transport }
    };
    let declare = Request::McpDeclare {
        workspace: workspace(),
        servers: vec![declared("browser-a", &["--x"]), declared("db", &["--new"])],
    };
    second.request(1, declare, &Body::empty()).await;
    assert_eq!(
        reply(&mut second).await,
        ReplyResult::Ok(Reply::McpDeclare {
            closed: vec!["db".to_owned(), "docs".to_owned()]
        })
    );
    let mut closed = fake.closed.lock().unwrap().clone();
    closed.sort();
    assert_eq!(closed, ["mcp-1", "mcp-2"]);
    // 期望集合里的不动：同一个配置接回同一个进程；缺的（新的 db）等按需连接时再起。
    assert_eq!(
        connection_of(&mut second, 2, connect_with("browser-a", &["--x"])).await,
        kept
    );
    assert_eq!(fake.connects.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_server_that_exited_is_started_again_not_reattached() {
    // 缺陷：服务器进程自己退出之后连接表里还留着它，同一配置再连（`-p` 的 dormant、HTTP 的重连）接回一个已经退出的进程，
    // 握手必然失败。现在看过 Closed 的连接不再接回：结束它，另起一个。
    let fake = Arc::new(FakeMcp::default());
    let server = mcp_server(fake.clone(), with_mcp()).await;
    let (mut client, _task) = client_as(&server, "tui-1").await;
    let first = connection_of(&mut client, 1, connect_with("browser-a", &["--x"])).await;
    let events = Request::McpEvents {
        connection: first.clone(),
    };
    client.request(2, events, &Body::empty()).await;
    fake.exit(0);
    let closed = event(&mut client).await;
    assert!(matches!(
        closed.item,
        StreamItem::Mcp {
            event: McpEvent::Closed { .. }
        }
    ));
    let again = connection_of(&mut client, 3, connect_with("browser-a", &["--x"])).await;
    assert_ne!(again, first);
    assert_eq!(fake.connects.lock().unwrap().len(), 2);
    assert_eq!(fake.closed.lock().unwrap().as_slice(), ["mcp-0"]);
}

/// 远端执行 F96（remote-exec.md §23.5）：新起的 stdio 服务器拿到这个客户端实例推来的环境变量（不进配置，也不进指纹：
/// 变量变了也接回同一个进程）。
#[tokio::test]
async fn a_new_server_gets_the_pushed_variables_and_reattaching_ignores_them() {
    let fake = Arc::new(FakeMcp::default());
    let mut settings = with_mcp();
    settings.capabilities = Capabilities::executor();
    let server = mcp_server(fake.clone(), settings).await;
    let (mut client, _task) = connect(&server);
    let first = Hello {
        session_env: Some(std::collections::BTreeMap::from([(
            "SSH_AUTH_SOCK".to_owned(),
            Some("/tmp/ssh-1/agent.1".to_owned()),
        )])),
        ..hello(1)
    };
    client.send(Kind::Hello, &first, &Body::empty()).await;
    client.frame().await.unwrap();
    let connection = connected(&mut client).await;
    let (_, request) = fake.connects.lock().unwrap()[0].clone();
    assert_eq!(
        request.env,
        std::collections::BTreeMap::from([(
            "SSH_AUTH_SOCK".to_owned(),
            Some("/tmp/ssh-1/agent.1".to_owned())
        )])
    );
    // 配置的原文不变（指纹按它算）。
    assert!(matches!(
        &request.transport,
        McpTransportConfig::Stdio { env, .. } if env.is_empty()
    ));
    // 换一条会话、变量变了：同一配置接回同一个进程。
    client.close().await;
    let (mut again, _again) = connect(&server);
    let second = Hello {
        session_env: Some(std::collections::BTreeMap::from([(
            "SSH_AUTH_SOCK".to_owned(),
            Some("/tmp/ssh-2/agent.2".to_owned()),
        )])),
        ..hello(2)
    };
    again.send(Kind::Hello, &second, &Body::empty()).await;
    again.frame().await.unwrap();
    assert_eq!(connected(&mut again).await, connection);
    assert_eq!(fake.connects.lock().unwrap().len(), 1);
}
