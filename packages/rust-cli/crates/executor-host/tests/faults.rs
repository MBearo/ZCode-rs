//! 端到端的故障注入（docs/specs/rust-cli/remote-exec.md §18.5、§18.7）：真实的 exec-client 与 exec-server（背后是 env-local），
//! 在故障注入的连接上：回复在路上丢了、断线后重连取回；流断线后按 seq 续上、输出完整有序；ack 乱序到达也照常收尾。

#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录与夹具、拼装客户端与服务端"
)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient, Reconnect};
use rust_cli_exec_server::{AllowAll, ExecServer, MemoryLedger, ServerConfig};
use rust_cli_exec_transport::fault::{Faults, Matcher, Switch, inject};
use rust_cli_exec_transport::memory::{MemoryConnector, MemoryListener, network};
use rust_cli_exec_transport::{Connector, Link, LinkError, Listener};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, OperationContext, Origin, StartCommand, WorkspaceRef, WriteFile,
    WritePrecondition,
};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId, ShellTaskId,
};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::mcp::NoMcpTransport;

/// 帧的类型字节（exec-wire 的表）：回复 0x11、事件 0x12、ack 0x13。
fn kind(code: u8) -> Matcher {
    Arc::new(move |frame: &[u8]| frame.first() == Some(&code))
}

/// 每接受一条连接按顺序取一份故障，记下开关。
struct FaultyListener {
    inner: MemoryListener,
    faults: Mutex<VecDeque<Faults>>,
    switches: Arc<Mutex<Vec<Switch>>>,
}

#[async_trait]
impl Listener for FaultyListener {
    async fn accept(&self) -> Result<Box<dyn Link>, LinkError> {
        let link = self.inner.accept().await?;
        let faults = self.faults.lock().unwrap().pop_front().unwrap_or_default();
        let (link, switch) = inject(link, faults);
        self.switches.lock().unwrap().push(switch);
        Ok(link)
    }
}

/// 客户端这一头的故障（每次连接都注入同一份）。
struct FaultyConnector {
    inner: MemoryConnector,
    faults: Faults,
}

#[async_trait]
impl Connector for FaultyConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        Ok(inject(self.inner.connect().await?, self.faults.clone()).0)
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    machine: MachineEnv,
    switches: Arc<Mutex<Vec<Switch>>>,
    ledger: Arc<MemoryLedger>,
    _server: ExecServer,
    _serving: rust_cli_base::OwnedTask<()>,
}

async fn setup(server_faults: Vec<Faults>, client_faults: Faults) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("work");
    std::fs::create_dir_all(&root).unwrap();
    let local = LocalEnvironment::new(None).with_shell(ShellConfig {
        scratch: base.join("scratch"),
        ledger: base.join("ledger"),
        bash: Some(PathBuf::from("/bin/bash")),
        bwrap: None,
        embedded: None,
        env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())].into(),
    });
    let ledger = Arc::new(MemoryLedger::default());
    let server = ExecServer::start(
        MachineEnv::new(Arc::new(local), Arc::new(NoMcpTransport)),
        ledger.clone(),
        Arc::new(AllowAll),
        ServerConfig::new("faults", std::env::consts::OS, std::env::consts::ARCH),
    )
    .await
    .unwrap();
    let (connector, listener) = network("faults");
    let switches = Arc::new(Mutex::new(Vec::new()));
    let listener = FaultyListener {
        inner: listener,
        faults: Mutex::new(server_faults.into()),
        switches: switches.clone(),
    };
    let serving = {
        let server = server.clone();
        rust_cli_base::owned(async move { server.serve(&listener).await })
    };
    let mut options = ClientOptions::new("faults", "client-1");
    options.heartbeat = Duration::from_millis(500);
    options.reconnect = Reconnect {
        attempts: 3,
        delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(10),
    };
    let connector = FaultyConnector {
        inner: connector,
        faults: client_faults,
    };
    let client = ExecClient::connect(Arc::new(connector), options)
        .await
        .unwrap();
    Setup {
        _dir: dir,
        root,
        machine: Arc::new(RemoteEnvironment::new(client)).machine(),
        switches,
        ledger,
        _server: server,
        _serving: serving,
    }
}

fn workspace(root: &std::path::Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    }
}

fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: Origin::User {
            session: Some(SessionId::new("s1")),
        },
    }
}

fn command(root: &std::path::Path, text: &str) -> StartCommand {
    StartCommand {
        command: text.to_owned(),
        cwd: EnvPath::new(root.to_string_lossy()),
        timeout_ms: Some(20_000),
        task: ShellTaskId::new("bfault1"),
        shell: Default::default(),
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: None,
        on_timeout: None,
        socket: None,
    }
}

async fn output_of(machine: &MachineEnv, operation: &str) -> String {
    let mut events = machine
        .commands()
        .watch_command(OperationId::new(operation), 0);
    let mut output = Vec::new();
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => output.extend(data),
            CommandEvent::Finished { .. } => return String::from_utf8(output).unwrap(),
            _ => {}
        }
    }
    panic!("no terminal event");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_lost_in_flight_is_fetched_after_reconnecting() {
    let setup = setup(vec![Faults::new().drop_sent(kind(0x11), 1)], Faults::new()).await;
    let path = setup.root.join("written.txt");
    let write = WriteFile {
        path: EnvPath::new(path.to_string_lossy()),
        bytes: b"once".to_vec(),
        precondition: WritePrecondition::Absent,
    };
    let files = setup.machine.files().clone();
    let ws = workspace(&setup.root);
    let call =
        rust_cli_base::owned(async move { files.write_file(&ws, context("op_w"), write).await });
    // 写已经做了、回复丢在路上：切断连接，客户端重连后按同一个调用 ID 取回。
    for _ in 0..200 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    setup.switches.lock().unwrap()[0].cut();
    let receipt = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!receipt.version.as_str().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"once");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_cut_midway_continues_by_seq_with_complete_output() {
    // 第一条连接发出 welcome、回复与几项事件之后切断。
    let setup = setup(vec![Faults::new().cut_after_sent(4)], Faults::new()).await;
    let text = "for i in $(seq 1 30); do echo line$i; sleep 0.01; done";
    setup
        .machine
        .commands()
        .start_command(
            &workspace(&setup.root),
            context("op_s"),
            command(&setup.root, text),
        )
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(20), output_of(&setup.machine, "op_s"))
        .await
        .unwrap();
    let expected: String = (1..=30).map(|i| format!("line{i}\n")).collect();
    assert_eq!(output, expected);
    assert!(
        setup.switches.lock().unwrap().len() >= 2,
        "the client reconnected"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reordered_acks_still_close_the_stream_and_empty_the_ledger() {
    let setup = setup(Vec::new(), Faults::new().hold_sent(kind(0x13))).await;
    let text = "for i in $(seq 1 10); do echo $i; sleep 0.01; done";
    setup
        .machine
        .commands()
        .start_command(
            &workspace(&setup.root),
            context("op_a"),
            command(&setup.root, text),
        )
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(20), output_of(&setup.machine, "op_a"))
        .await
        .unwrap();
    assert_eq!(output.lines().count(), 10);
    // 之后再做一次调用：被扣住的 ack 随它放出；最后账本里没有剩下的调用与事件。
    let ws = workspace(&setup.root);
    setup.machine.info().facts(&ws).await.unwrap();
    for _ in 0..200 {
        if setup.ledger.is_empty() {
            break;
        }
        setup.machine.info().path_facts(&ws).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(setup.ledger.is_empty(), "{} calls left", setup.ledger.len());
}
