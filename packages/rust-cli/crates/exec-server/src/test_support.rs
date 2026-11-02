//! 单测用的执行环境替身与脚本化的客户端。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_exec_transport::memory::{MemoryLink, pair};
use rust_cli_exec_transport::{FrameSink, FrameSource, Link};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::{OperationContext, Origin, Workspace};
use rust_cli_exec_wire::envelope::{ReplyHeader, RequestHeader};
use rust_cli_exec_wire::frame::{Frame, Kind, Reassembler, Received};
use rust_cli_exec_wire::handshake::{Capabilities, Hello, PROTOCOL};
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, FileVersion, OperationContext as Ctx, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_command::EnvOverrides;
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::machine::{CommandEvents, Commands, EnvInfo, Files, MachineEnv};
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, NoMcpTransport,
};
use rust_cli_kernel_api::testing::UnusedEnvironment;
use serde::Serialize;
use tokio::sync::{Notify, mpsc};

use crate::{AllowAll, ExecLedger, ExecServer, Launcher, MemoryLedger, ServerConfig};

/// 执行环境的替身：写文件计数（看有没有重跑）、可以卡住等测试放行；`watch_command` 的事件由测试推入；记下每条起的命令
/// 与每个连上的 MCP 服务器带的推来的环境变量（F96，经 [`TestLauncher`]）。
#[derive(Default)]
pub(crate) struct TestMachine {
    pub writes: AtomicUsize,
    pub gate: Option<Arc<Notify>>,
    pub events: Mutex<Option<mpsc::UnboundedReceiver<Result<CommandEvent, EnvError>>>>,
    pub started: Mutex<Vec<EnvOverrides>>,
    pub mcp_started: Mutex<Vec<EnvOverrides>>,
}

/// 起进程的入口：命令交给 `TestMachine` 的端口、MCP 交给给的传输，推来的变量记在 `TestMachine` 里。
pub(crate) struct TestLauncher {
    machine: Arc<TestMachine>,
    mcp: Arc<dyn McpTransport>,
}

#[async_trait]
impl Launcher for TestLauncher {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: Ctx,
        request: StartCommand,
        env: &EnvOverrides,
    ) -> Result<CommandEvents, EnvError> {
        self.machine.started.lock().unwrap().push(env.clone());
        self.machine
            .start_command(workspace, context, request)
            .await
    }

    async fn connect_mcp(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
        env: &EnvOverrides,
    ) -> Result<McpConnectionId, McpError> {
        self.machine.mcp_started.lock().unwrap().push(env.clone());
        self.mcp.connect(workspace, request).await
    }
}

#[async_trait]
impl EnvInfo for TestMachine {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Ok(EnvFacts {
            cwd: workspace.root.clone(),
            platform: "linux".to_owned(),
            os_version: "Linux 6".to_owned(),
            shell: "bash".to_owned(),
            is_git_repo: false,
            home: None,
            uid: None,
        })
    }
}

#[async_trait]
impl Files for TestMachine {
    async fn read_file(
        &self,
        _: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        Ok(FileContent {
            bytes: format!("content of {}", request.path.as_str()).into_bytes(),
            version: FileVersion::new("v1"),
        })
    }

    async fn write_file(
        &self,
        _: &WorkspaceRef,
        _: Ctx,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        let count = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(WriteReceipt {
            version: FileVersion::new(format!("v{count}:{}", request.bytes.len())),
        })
    }
}

#[async_trait]
impl Commands for TestMachine {
    async fn start_command(
        &self,
        _: &WorkspaceRef,
        context: Ctx,
        request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        let _ = request;
        Ok(self.watch_command(context.operation, 0))
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        match self.events.lock().unwrap().take() {
            Some(mut rx) => futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed(),
            None => futures_util::stream::empty().boxed(),
        }
    }

    async fn cancel(&self, _: OperationId) -> Result<(), EnvError> {
        Ok(())
    }

    async fn close_session(&self, _: &SessionId, _: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

impl TestMachine {
    /// 只换上环境事实、文件与命令三个端口的机器（其余端口返回 `Unsupported`），MCP 的管道是 `mcp`。
    pub(crate) fn machine(
        self: Arc<Self>,
        mcp: Arc<dyn rust_cli_kernel_api::mcp::McpTransport>,
    ) -> MachineEnv {
        MachineEnv::new(Arc::new(UnusedEnvironment), mcp)
            .with_info(self.clone())
            .with_files(self.clone())
            .with_commands(self)
    }
}

pub(crate) fn config() -> ServerConfig {
    let mut config = ServerConfig::new("0.1.0-test", "linux", "x86_64");
    config.home = Some("/home/dev".to_owned());
    config
}

pub(crate) async fn server_with(
    machine: Arc<TestMachine>,
    ledger: Arc<dyn ExecLedger>,
    config: ServerConfig,
) -> ExecServer {
    server_with_mcp(machine, Arc::new(NoMcpTransport), ledger, config).await
}

/// 同 [`server_with`]，MCP 的管道用给的传输（R5）。
pub(crate) async fn server_with_mcp(
    machine: Arc<TestMachine>,
    mcp: Arc<dyn rust_cli_kernel_api::mcp::McpTransport>,
    ledger: Arc<dyn ExecLedger>,
    config: ServerConfig,
) -> ExecServer {
    let launcher = Arc::new(TestLauncher {
        machine: machine.clone(),
        mcp: mcp.clone(),
    });
    ExecServer::start(
        (machine.machine(mcp), launcher),
        ledger,
        Arc::new(AllowAll),
        config,
    )
    .await
    .unwrap()
}

pub(crate) async fn server(machine: Arc<TestMachine>) -> (ExecServer, Arc<MemoryLedger>) {
    let ledger = Arc::new(MemoryLedger::default());
    (server_with(machine, ledger.clone(), config()).await, ledger)
}

/// 连上服务端：返回客户端这一头与服务这条连接的任务。
pub(crate) fn connect(server: &ExecServer) -> (Client, rust_cli_base::OwnedTask<()>) {
    let (client, served): (MemoryLink, MemoryLink) = pair("exec");
    let server = server.clone();
    let task = rust_cli_base::owned(async move {
        let _ = server.serve_link(Box::new(served)).await;
    });
    let (sink, source) = Box::new(client).split();
    (
        Client {
            sink,
            source,
            reassembler: Reassembler::default(),
        },
        task,
    )
}

pub(crate) fn hello(epoch: u64) -> Hello {
    Hello {
        protocol: PROTOCOL,
        client_version: "0.1.0-test".to_owned(),
        client_instance: "client-1".to_owned(),
        epoch,
        capabilities: Capabilities::ports(),
        heartbeat_ms: None,
        credentials: None,
        session_env: None,
    }
}

pub(crate) fn workspace() -> Workspace {
    Workspace {
        environment: "local".to_owned(),
        root: "/w".to_owned(),
    }
}

pub(crate) fn write(
    path: &str,
    body: &mut rust_cli_exec_wire::BodyBuilder,
    data: &[u8],
) -> Request {
    Request::WriteFile {
        workspace: workspace(),
        context: OperationContext {
            environment: "local".to_owned(),
            generation: "g".to_owned(),
            owner_epoch: 1,
            operation: format!("op:{path}"),
            origin: Origin::User { session: None },
        },
        path: path.to_owned(),
        bytes: body.push(data),
        precondition: rust_cli_exec_wire::dto::files::WritePrecondition::Any,
    }
}

/// 客户端这一头。
pub(crate) struct Client {
    sink: Box<dyn FrameSink>,
    source: Box<dyn FrameSource>,
    reassembler: Reassembler,
}

impl Client {
    pub async fn send<T: Serialize>(&mut self, kind: Kind, header: &T, body: &Body) {
        self.sink
            .send(Frame::encode(kind, header, body).unwrap())
            .await
            .unwrap();
    }

    /// 下一帧；连接结束时 None。
    pub async fn frame(&mut self) -> Option<Frame> {
        loop {
            let bytes = self.source.recv().await.ok()??;
            match self.reassembler.push(bytes).unwrap() {
                Received::Frame(frame) => return Some(frame),
                Received::Pending | Received::Unknown(_) => {}
            }
        }
    }

    pub async fn handshake(&mut self, epoch: u64) -> Frame {
        self.send(Kind::Hello, &hello(epoch), &Body::empty()).await;
        self.frame().await.unwrap()
    }

    pub async fn request(&mut self, call: u64, request: Request, body: &Body) {
        let header = RequestHeader {
            call,
            request,
            resume_after: None,
        };
        self.send(Kind::Request, &header, body).await;
    }

    pub async fn reply(&mut self) -> (ReplyHeader, Body) {
        loop {
            let frame = self.frame().await.expect("connection ended");
            if frame.kind == Kind::Reply {
                return (frame.header().unwrap(), frame.body);
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.sink.close().await;
    }
}
