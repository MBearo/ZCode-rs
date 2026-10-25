//! 远端 MCP 与常驻执行器，真实进程（docs/specs/rust-cli/remote-exec.md §22.5、§22.10）：服务器（仓库里的 bash 夹具）由
//! 常驻执行器起；杀掉代理（ssh 断开）后同一个服务器进程还在、断线期间的回答补发过来；换一个客户端（另一个 TUI 进程）
//! 同一份配置接回同一个进程，改了配置重启，期望集合里没有它时结束；常驻执行器重启后事件流以 Closed 结束。
//!
//! 每个测试用自己的临时 HOME；结束时 `zcode executor stop`，不留常驻进程与服务器进程。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、拉起执行器与代理进程、发信号、检查进程"
)]

mod resident;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpTransport, McpTransportConfig, McpTransportEvent,
};
use serde_json::{Value, json};

use resident::{Home, Proxies, client, kill};

fn fixture_script() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-shell.sh")
        .display()
        .to_string()
}

fn server(extra: &[&str]) -> McpConnect {
    let mut args = vec![fixture_script()];
    args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    McpConnect {
        server: "fx".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "/bin/bash".to_owned(),
            args,
            env: [("MCP_PROBE".to_owned(), "${HOME}".to_owned())].into(),
            cwd: None,
        },
        env: Default::default(),
    }
}

/// 一条 MCP 连接：发请求，按 id 等回答。
struct Session {
    transport: Arc<RemoteEnvironment>,
    connection: McpConnectionId,
    events: BoxStream<'static, McpTransportEvent>,
    next: u64,
}

impl Session {
    async fn open(transport: Arc<RemoteEnvironment>, home: &Home, extra: &[&str]) -> Self {
        let connection = transport
            .connect(&home.workspace(), server(extra))
            .await
            .unwrap();
        let events = transport.events(&connection);
        let mut session = Self {
            transport,
            connection,
            events,
            next: 0,
        };
        session.call("initialize", json!({})).await;
        session
    }

    async fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.transport
            .send(&self.connection, message)
            .await
            .unwrap();
        id
    }

    async fn answer(&mut self, id: u64) -> Value {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(30), self.events.next())
                .await
                .unwrap_or_else(|_| panic!("no answer in time"))
                .unwrap_or_else(|| panic!("the stream ended"));
            match event {
                McpTransportEvent::Message { message } if message["id"] == id => return message,
                McpTransportEvent::Message { .. } => {}
                closed @ McpTransportEvent::Closed { .. } => panic!("closed: {closed:?}"),
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.answer(id).await
    }

    /// `where` 工具的回答里的一项。
    async fn where_is(&mut self, key: &str) -> String {
        let answer = self
            .call("tools/call", json!({"name": "where", "arguments": {}}))
            .await;
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap()
            .to_owned()
    }
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 等一个进程结束（测试里的等待；产品代码不轮询）。
async fn gone(pid: &str) {
    for _ in 0..400 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("process {pid} is still running");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_survives_a_lost_proxy_and_its_answer_is_replayed() {
    let home = Home::new();
    let proxies = Proxies::new(&home);
    let client = client(&proxies, "c1").await;
    let instance = client.welcome().unwrap().server_instance;
    let transport = Arc::new(RemoteEnvironment::new(client.clone()));
    let mut session = Session::open(transport, &home, &[]).await;
    let pid = session.where_is("pid").await;
    // `${HOME}` 由执行器用它自己的环境展开。
    assert_eq!(
        session.where_is("probe").await,
        home.env().iter().find(|(key, _)| key == "HOME").unwrap().1
    );
    // 一个要等两秒的调用发出去之后断开通道：代理死了，常驻进程与服务器照常，回答等重连后补发。
    let slow = session
        .send(
            "tools/call",
            json!({"name": "slow", "arguments": {"seconds": 2}}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    kill(proxies.last());
    let answer = session.answer(slow).await;
    assert_eq!(answer["result"]["content"][0]["text"], "slept 2");
    assert!(
        proxies.count() >= 2,
        "the client reconnected through a new proxy"
    );
    assert_eq!(client.welcome().unwrap().server_instance, instance);
    assert_eq!(
        session.where_is("pid").await,
        pid,
        "the same server process"
    );
    session.transport.close(&session.connection).await.unwrap();
    gone(&pid).await;
    client.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_client_reattaches_the_server_and_a_changed_config_restarts_it() {
    let home = Home::new();
    let proxies = Proxies::new(&home);
    let first = client(&proxies, "tui-1").await;
    let mut session =
        Session::open(Arc::new(RemoteEnvironment::new(first.clone())), &home, &[]).await;
    let pid = session.where_is("pid").await;
    first.close().await;
    // 另一个进程（新的客户端实例）连上：先声明期望集合（配置相同的不动），再连同一份配置——接回同一个进程。
    let second = client(&proxies, "tui-2").await;
    let transport = Arc::new(RemoteEnvironment::new(second.clone()));
    let closed = transport
        .declare_mcp(&home.workspace(), vec![server(&[])])
        .await
        .unwrap();
    assert!(closed.is_empty(), "{closed:?}");
    let mut again = Session::open(transport.clone(), &home, &[]).await;
    assert_eq!(
        again.where_is("pid").await,
        pid,
        "reattached, not restarted"
    );
    // 改了配置：结束旧的、起新的。
    let mut changed = Session::open(transport.clone(), &home, &["--v2"]).await;
    let new_pid = changed.where_is("pid").await;
    assert_ne!(new_pid, pid);
    gone(&pid).await;
    // 期望集合里没有它（配置删了）：结束。
    let closed = transport
        .declare_mcp(&home.workspace(), Vec::new())
        .await
        .unwrap();
    assert_eq!(closed, ["fx"]);
    gone(&new_pid).await;
    second.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_resident_ends_the_server_and_its_stream() {
    let home = Home::new();
    let proxies = Proxies::new(&home);
    let client = client(&proxies, "c1").await;
    let mut session =
        Session::open(Arc::new(RemoteEnvironment::new(client.clone())), &home, &[]).await;
    let pid = session.where_is("pid").await;
    let status = home
        .wait_for("the resident", |status| !status["resident"].is_null())
        .await;
    kill(u32::try_from(status["resident"]["pid"].as_u64().unwrap()).unwrap());
    // 客户端经新的代理连上新拉起的常驻进程：旧的流以 Closed 结束（ext 照服务器退出处理），服务器随旧执行器结束。
    let closed = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = session.events.next().await {
            if let McpTransportEvent::Closed { reason, .. } = event {
                return reason;
            }
        }
        None
    })
    .await
    .unwrap();
    assert!(
        closed
            .as_deref()
            .is_some_and(|reason| reason.contains("executor restarted")),
        "{closed:?}"
    );
    gone(&pid).await;
    client.close().await;
}
