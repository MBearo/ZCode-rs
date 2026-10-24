//! 常驻执行器（docs/specs/rust-cli/remote-exec.md §19.5）：拿单实例锁、监听本用户私有的 socket，一直跑到 `stop`
//! （或入口给的关闭信号）。每条连接先看第一帧：`admin` 在这里回答（状态、停止、空闲时停止）；其余交给 exec-server。
//!
//! 单实例锁是“有没有常驻进程”的唯一事实：socket 文件可能是上次崩溃留下的，只有拿到锁的进程才删它。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rust_cli_base::TaskScope;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_exec_server::{AllowAll, ExecServer, ServerConfig};
use rust_cli_exec_transport::stream::stream_link;
use rust_cli_exec_transport::{FrameSink, FrameSource, Link, LinkError, MAX_FRAME};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::admin::{
    AdminAction, AdminOutcome, AdminReply, AdminRequest, ExecutorStatus,
};
use rust_cli_exec_wire::frame::{Decoded, Frame, Kind, decode};
use rust_cli_exec_wire::handshake::PROTOCOL;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::mcp::NoMcpTransport;
use rust_cli_paths::ExecutorPaths;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::file_ledger::{FileLedger, Retention};

/// 等第一帧的时间（同 exec-server 等 `hello`）。
const FIRST_FRAME: Duration = Duration::from_secs(30);

/// 常驻执行器的设置（入口给）。
pub struct ResidentConfig {
    pub paths: ExecutorPaths,
    pub version: String,
    /// 自己的可执行文件（状态里报告，部署与升级时用）。
    pub executable: Option<String>,
    pub clock: Arc<dyn Clock>,
    pub retention: Retention,
    /// 账本多久清理一次（§19.7：10 分钟）。
    pub ledger_sweep: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// 已经有一个常驻执行器（它拿着单实例锁）。
    #[error("another executor is already running")]
    AlreadyRunning,
    #[error("{0}")]
    Failed(String),
}

/// 新连接的闸门：停止之后不再放进来；放进来、还没结束的连接也算“不空闲”（握手中的连接还没登记成客户端）。
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    stop: CancellationToken,
}

#[derive(Default)]
struct GateState {
    stopping: bool,
    admitted: u64,
}

struct Admission(Arc<Gate>);

impl Drop for Admission {
    fn drop(&mut self) {
        self.0.lock().admitted -= 1;
    }
}

impl Gate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn admit(self: &Arc<Self>) -> Option<Admission> {
        let mut state = self.lock();
        if state.stopping {
            return None;
        }
        state.admitted += 1;
        Some(Admission(self.clone()))
    }

    fn stop(&self) {
        self.lock().stopping = true;
        self.stop.cancel();
    }

    /// 判断与停止在同一把锁里（§19.5）：判断之后不会有新连接进来。返回判断时的状态与是否停了。
    fn stop_if_idle(&self, status: impl FnOnce(u64) -> ExecutorStatus) -> (ExecutorStatus, bool) {
        let mut state = self.lock();
        let status = status(state.admitted);
        if status.idle {
            state.stopping = true;
            drop(state);
            self.stop.cancel();
        }
        let idle = status.idle;
        (status, idle)
    }
}

struct Shared {
    server: ExecServer,
    env: Arc<LocalEnvironment>,
    gate: Arc<Gate>,
    version: String,
    executable: Option<String>,
    started: Instant,
}

impl Shared {
    fn status(&self, admitted: u64) -> ExecutorStatus {
        let activity = self.server.activity();
        let busy = self.env.busy();
        let idle = admitted == 0
            && activity.clients == 0
            && activity.calls == 0
            && activity.streams == 0
            && busy.commands == 0
            && busy.notices == 0;
        ExecutorStatus {
            protocol: PROTOCOL,
            version: self.version.clone(),
            instance: self.server.instance().to_owned(),
            pid: std::process::id(),
            executable: self.executable.clone(),
            uptime_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            clients: activity.clients.max(admitted),
            calls: activity.calls,
            streams: activity.streams,
            commands: busy.commands,
            notices: busy.notices,
            idle,
        }
    }
}

/// 起来了的常驻执行器（拿着锁、在监听）。
pub struct Resident {
    _lock: rust_cli_fs::ExclusiveLock,
    listener: rust_cli_net::uds::UdsListener,
    socket: PathBuf,
    ledger: Arc<FileLedger>,
    sweep: Duration,
    shared: Arc<Shared>,
}

impl Resident {
    /// 拿锁、打开账本（恢复）、监听。已经有一个在跑时是 [`StartError::AlreadyRunning`]。
    pub async fn start(
        config: ResidentConfig,
        env: Arc<LocalEnvironment>,
        server: ServerConfig,
    ) -> Result<Self, StartError> {
        let failed = |what: &str, error: &dyn std::fmt::Display| {
            StartError::Failed(format!("{what}: {error}"))
        };
        let dir = config.paths.dir().as_path();
        rust_cli_fs::create_private_dir_all(dir)
            .await
            .map_err(|e| failed("creating the executor directory", &e))?;
        let lock = rust_cli_fs::try_lock_exclusive(config.paths.lock().as_path())
            .await
            .map_err(|e| failed("taking the executor lock", &e))?
            .ok_or(StartError::AlreadyRunning)?;
        let socket = config.paths.socket().as_path().to_path_buf();
        crate::admin::check_socket_path(&socket).map_err(|e| StartError::Failed(e.to_string()))?;
        let ledger = Arc::new(
            FileLedger::open(
                config.paths.ledger().as_path(),
                config.clock.clone(),
                config.retention,
            )
            .await
            .map_err(|e| failed("opening the ledger", &e))?,
        );
        let machine = MachineEnv::new(env.clone(), Arc::new(NoMcpTransport));
        let server = ExecServer::start(machine, ledger.clone(), Arc::new(AllowAll), server)
            .await
            .map_err(|e| failed("starting the executor", &e))?;
        // 拿着锁：路径上的旧文件只可能是上次留下的。
        rust_cli_fs::remove_optional(&socket)
            .await
            .map_err(|e| failed("removing the old socket", &e))?;
        let listener = rust_cli_net::uds::UdsListener::bind(&socket)
            .map_err(|e| failed(&format!("listening on {}", socket.display()), &e))?;
        rust_cli_fs::restrict_to_owner(&socket)
            .await
            .map_err(|e| failed("restricting the socket", &e))?;
        tracing::info!(target: "executor_host", socket = %socket.display(), instance = %server.instance(), "resident executor listening");
        Ok(Self {
            _lock: lock,
            listener,
            socket,
            ledger,
            sweep: config.ledger_sweep,
            shared: Arc::new(Shared {
                server,
                env,
                gate: Arc::new(Gate::default()),
                version: config.version,
                executable: config.executable,
                started: Instant::now(),
            }),
        })
    }

    /// socket 的路径。
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// 接受连接直到 `stop`（管理帧）或 `shutdown`；然后断开全部连接、删掉 socket 文件、放开锁。
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        let scope = TaskScope::root("executor-host/resident", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "resident task panicked");
        });
        let spawner = scope.spawner();
        let mut sweep = tokio::time::interval(self.sweep);
        sweep.tick().await;
        tokio::pin!(shutdown);
        let reason = loop {
            tokio::select! {
                accepted = self.listener.accept_raw() => match accepted {
                    Ok(halves) => {
                        let link = stream_link(halves.read, halves.write, "socket", MAX_FRAME);
                        let shared = self.shared.clone();
                        drop(spawner.spawn("connection", handle(shared, Box::new(link))));
                    }
                    Err(error) => tracing::warn!(target: "executor_host", %error, "accept failed"),
                },
                () = self.shared.gate.stop.cancelled() => break "stop requested",
                () = &mut shutdown => break "signal",
                _ = sweep.tick() => {
                    if let Err(error) = self.ledger.sweep().await {
                        tracing::warn!(target: "executor_host", %error, "ledger sweep failed");
                    }
                }
            }
        };
        tracing::info!(target: "executor_host", reason, "resident executor stopping");
        self.shared.gate.stop();
        let Self {
            _lock,
            listener,
            socket,
            shared,
            ..
        } = self;
        // 先关监听（不再接受连接），再删 socket 文件，锁最后放开。F94：原来写 `drop(listener)`，Windows 上
        // net 的 `UdsListener` 是不持有资源的占位（那里拿不到监听），clippy 的 `drop_non_drop` 报错；
        // 改用 `close` 写明“关闭监听”这个原意，两个平台一样。
        listener.close();
        if let Err(error) = rust_cli_fs::remove_optional(&socket).await {
            tracing::warn!(target: "executor_host", %error, "removing the socket failed");
        }
        // 断开连接、结束服务端的任务；执行环境随最后一个引用释放（按 env-local 的规则结束还在跑的命令）。
        drop(scope);
        drop(shared);
        drop(_lock);
    }
}

/// 一条连接：第一帧是 `admin` 就在这里回答；否则把第一帧放回去，交给 exec-server。
async fn handle(shared: Arc<Shared>, link: Box<dyn Link>) {
    let peer = link.peer();
    let (mut sink, mut source) = link.split();
    let first = match tokio::time::timeout(FIRST_FRAME, source.recv()).await {
        Ok(Ok(Some(bytes))) => bytes,
        _ => return,
    };
    if let Ok(Decoded::Frame(frame)) = decode(first.clone())
        && frame.kind == Kind::Admin
    {
        let action = frame
            .header::<AdminRequest>()
            .map_or(AdminAction::Status, |request| request.action);
        let reply = admin(&shared, action);
        if let Ok(payload) = Frame::encode(Kind::AdminReply, &reply, &Body::empty()) {
            let _ = sink.send(payload).await;
        }
        let _ = sink.close().await;
        return;
    }
    let Some(admission) = shared.gate.admit() else {
        let _ = sink.close().await;
        return;
    };
    let link = Rejoined {
        sink,
        source: Box::new(Prepend {
            first: Some(first),
            inner: source,
        }),
        peer,
    };
    if let Err(error) = shared.server.serve_link(Box::new(link)).await {
        tracing::warn!(target: "executor_host", %error, "connection ended with an error");
    }
    drop(admission);
}

fn admin(shared: &Shared, action: AdminAction) -> AdminReply {
    match action {
        AdminAction::Stop => {
            let status = shared.status(shared.gate.lock().admitted);
            shared.gate.stop();
            AdminReply {
                outcome: AdminOutcome::Stopping,
                status,
            }
        }
        AdminAction::StopIfIdle => {
            let (status, stopped) = shared.gate.stop_if_idle(|admitted| shared.status(admitted));
            AdminReply {
                outcome: if stopped {
                    AdminOutcome::Stopping
                } else {
                    AdminOutcome::Busy
                },
                status,
            }
        }
        AdminAction::Status | AdminAction::Other => AdminReply {
            outcome: AdminOutcome::Status,
            status: shared.status(shared.gate.lock().admitted),
        },
    }
}

/// 收发两端重新拼成一条连接。
struct Rejoined {
    sink: Box<dyn FrameSink>,
    source: Box<dyn FrameSource>,
    peer: String,
}

impl Link for Rejoined {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        (self.sink, self.source)
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }
}

/// 先交出已经读到的第一帧，再从原来的接收端读。
struct Prepend {
    first: Option<Bytes>,
    inner: Box<dyn FrameSource>,
}

#[async_trait]
impl FrameSource for Prepend {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        if let Some(first) = self.first.take() {
            return Ok(Some(first));
        }
        self.inner.recv().await
    }
}
