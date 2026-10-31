//! 执行器的服务端（remote-exec.md §15.3、§18）：收连接、握手与鉴权、把请求派发给注入的 `MachineEnv`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use rust_cli_base::{Spawner, TaskScope, units};
use rust_cli_exec_transport::{Link, LinkError, Listener};
use rust_cli_exec_wire::dto::core::PathStyle;
use rust_cli_exec_wire::frame::PART_SIZE;
use rust_cli_exec_wire::handshake::Capabilities;
use rust_cli_kernel_api::machine::MachineEnv;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::auth::Authenticator;
use crate::calls::StreamControl;
use crate::fence::Fence;
use crate::ledger::{CallKey, ExecLedger, LedgerError};

/// 缺省：客户端实例断开多久后关掉它的流、删掉它只在内存里的记录（§19.7）。
const DEFAULT_ABANDON_AFTER: Duration = Duration::from_secs(units::SECONDS_PER_DAY);
/// 缺省：多久查一次断开太久的实例。
const DEFAULT_SWEEP_EVERY: Duration = Duration::from_secs(10 * units::SECONDS_PER_MINUTE);

/// 执行器报告给客户端的事实与服务端的设置（由执行器入口给）。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub server_version: String,
    pub os: String,
    pub arch: String,
    pub path_style: PathStyle,
    pub home: Option<String>,
    pub shell: Option<String>,
    /// 报告的能力位（R1：`MachineEnv` 的 8 个端口；R5 的执行器另报告 `mcp`，§22.4）。
    pub capabilities: Capabilities,
    pub fence: Fence,
    pub part_size: usize,
    /// 客户端实例断开多久后关掉它的流、删掉它只在内存里的记录（§19.7；缺省 24 小时）。
    pub abandon_after: Duration,
    /// 多久查一次断开太久的实例（缺省 10 分钟）。
    pub sweep_every: Duration,
}

impl ServerConfig {
    pub fn new(
        server_version: impl Into<String>,
        os: impl Into<String>,
        arch: impl Into<String>,
    ) -> Self {
        Self {
            server_version: server_version.into(),
            os: os.into(),
            arch: arch.into(),
            // 服务端就跑在执行的那台机器上：编译目标就是它的路径风格。
            path_style: if cfg!(windows) {
                PathStyle::Windows
            } else {
                PathStyle::Posix
            },
            home: None,
            shell: None,
            capabilities: Capabilities::ports(),
            fence: Fence::Open,
            part_size: PART_SIZE,
            abandon_after: DEFAULT_ABANDON_AFTER,
            sweep_every: DEFAULT_SWEEP_EVERY,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

/// 服务端此刻的活动（常驻执行器的状态与“空闲”判断，§19.5、§19.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Activity {
    /// 连着的客户端实例数。
    pub clients: u64,
    /// 正在执行的调用数。
    pub calls: u64,
    /// 开着的流数（断线的实例的流也算；MCP 连接的事件流也在这里，所以有 MCP 服务器在跑时不算空闲，§22.5）。
    pub streams: u64,
}

/// 一次正在执行的调用：建时加一，丢掉时减一（任务被中止也会丢掉）。
pub(crate) struct Running(Arc<AtomicU64>);

impl Running {
    pub fn start(inner: &Inner) -> Self {
        inner.running.fetch_add(1, Ordering::SeqCst);
        Self(inner.running.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 一个客户端实例当前的连接：回复与事件都发到这里（没有连接时只留在账本里）。
pub(crate) struct Route {
    pub epoch: u64,
    pub data: mpsc::Sender<Bytes>,
}

pub(crate) struct Inner {
    pub machine: MachineEnv,
    pub ledger: Arc<dyn ExecLedger>,
    pub auth: Arc<dyn Authenticator>,
    pub config: ServerConfig,
    pub instance: String,
    routes: Mutex<HashMap<String, Route>>,
    streams: Mutex<HashMap<CallKey, StreamControl>>,
    /// 正在执行的调用数。
    running: Arc<AtomicU64>,
    /// 断开的客户端实例与断开的时刻（重新连上时删掉）。
    away: Mutex<HashMap<String, Instant>>,
    /// MCP 的连接表（R5，remote-exec.md §22.5）。
    pub mcp: crate::mcp::McpTable,
    /// 按客户端实例推来的环境变量（F96，remote-exec.md §23.5）。
    pub env: crate::env::EnvTable,
    /// 起好、还没被线上的 watch_command 接走的命令流（工作线 F119，remote-exec.md §18.5）。
    pub started: crate::started::Started,
    pub spawner: Spawner,
}

impl Inner {
    pub fn routes(&self) -> MutexGuard<'_, HashMap<String, Route>> {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn streams(&self) -> MutexGuard<'_, HashMap<CallKey, StreamControl>> {
        self.streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn away(&self) -> MutexGuard<'_, HashMap<String, Instant>> {
        self.away
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 一个客户端实例连上了。
    pub fn connected(&self, instance: &str) {
        self.away().remove(instance);
    }

    /// 一个客户端实例的连接结束了（没有被更新的连接替换）。
    pub fn disconnected(&self, instance: &str) {
        self.away().insert(instance.to_owned(), Instant::now());
    }

    /// 断开超过 `abandon_after` 的实例：关掉它们的流、删掉它们只在内存里的记录（§19.7）。返回清掉的实例。
    pub async fn sweep_abandoned(&self) -> Vec<String> {
        let limit = self.config.abandon_after;
        let gone: Vec<String> = {
            let mut away = self.away();
            let gone: Vec<String> = away
                .iter()
                .filter(|(_, since)| since.elapsed() >= limit)
                .map(|(instance, _)| instance.clone())
                .collect();
            for instance in &gone {
                away.remove(instance);
            }
            gone
        };
        for instance in &gone {
            let streams: Vec<StreamControl> = {
                let mut streams = self.streams();
                let keys: Vec<CallKey> = streams
                    .keys()
                    .filter(|key| &key.instance == instance)
                    .cloned()
                    .collect();
                keys.iter().filter_map(|key| streams.remove(key)).collect()
            };
            drop(streams);
            // 它的 MCP 服务器一起结束（R5，§22.5）；它推来的环境变量一起忘掉（F96）。
            crate::mcp::forget(self, instance).await;
            self.env.forget(instance);
            self.started.forget(instance);
            if let Err(error) = self.ledger.forget_client(instance).await {
                tracing::warn!(target: "exec_server", %error, "forgetting an abandoned client failed");
            }
            tracing::info!(target: "exec_server", client = %instance, "abandoned client forgotten");
        }
        gone
    }

    /// 发给一个客户端实例当前的连接；没有连接时不发（留在账本里，等补发）。
    pub async fn send_to(&self, instance: &str, payload: Bytes) {
        let data = self.routes().get(instance).map(|route| route.data.clone());
        if let Some(data) = data {
            let _ = data.send(payload).await;
        }
    }
}

/// 执行器的服务端。可以克隆；最后一份被丢掉时，它启动的执行与流都结束。
#[derive(Clone)]
pub struct ExecServer {
    pub(crate) inner: Arc<Inner>,
    _scope: Arc<TaskScope>,
}

impl ExecServer {
    /// 建服务端：先按账本恢复（执行器重启前没有终态的调用记成中断）。
    pub async fn start(
        machine: MachineEnv,
        ledger: Arc<dyn ExecLedger>,
        auth: Arc<dyn Authenticator>,
        config: ServerConfig,
    ) -> Result<Self, ServerError> {
        let interrupted = ledger.recover().await?;
        if !interrupted.is_empty() {
            tracing::info!(target: "exec_server", count = interrupted.len(), "calls without a final state were marked interrupted");
        }
        let scope = TaskScope::root("exec-server", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "executor task panicked");
        });
        let inner = Arc::new(Inner {
            machine,
            ledger,
            auth,
            config,
            instance: format!("{:016x}", rust_cli_base::random::u64()),
            routes: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            running: Arc::new(AtomicU64::new(0)),
            away: Mutex::new(HashMap::new()),
            mcp: crate::mcp::McpTable::default(),
            env: crate::env::EnvTable::default(),
            started: crate::started::Started::default(),
            spawner: scope.spawner(),
        });
        // 回收断开太久的实例（只回收，不同步状态）；任务属于服务端的作用域，服务端释放时结束。
        let sweeping = Arc::downgrade(&inner);
        let every = inner.config.sweep_every;
        drop(inner.spawner.spawn("sweep", async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(inner) = sweeping.upgrade() else {
                    return;
                };
                inner.sweep_abandoned().await;
            }
        }));
        Ok(Self {
            inner,
            _scope: Arc::new(scope),
        })
    }

    /// 此刻的活动（§19.5 的“空闲”判断用）。
    pub fn activity(&self) -> Activity {
        Activity {
            clients: self.inner.routes().len() as u64,
            calls: self.inner.running.load(Ordering::SeqCst),
            streams: self.inner.streams().len() as u64,
        }
    }

    /// 这个执行器实例的名字（`welcome.server_instance`）。
    pub fn instance(&self) -> &str {
        &self.inner.instance
    }

    /// 服务一条连接，直到它结束（断开、心跳超时、违反协议或被同一实例的新连接替换）。
    pub async fn serve_link(&self, link: Box<dyn Link>) -> Result<(), ServerError> {
        crate::connection::serve(self.inner.clone(), link).await
    }

    /// 接受连接，每条一个任务；监听关闭时返回。
    pub async fn serve(&self, listener: &dyn Listener) {
        loop {
            match listener.accept().await {
                Ok(link) => {
                    let inner = self.inner.clone();
                    drop(self.inner.spawner.spawn("connection", async move {
                        if let Err(error) = crate::connection::serve(inner, link).await {
                            tracing::warn!(target: "exec_server", %error, "connection ended with an error");
                        }
                    }));
                }
                Err(LinkError::Closed) => return,
                Err(error) => tracing::warn!(target: "exec_server", %error, "accept failed"),
            }
        }
    }
}
