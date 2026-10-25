//! 连接状态机的驱动（remote-exec.md §19.10）：跑部署与握手，订阅 exec-client 的连接状态，把状态发到一个 `watch`。
//! 状态只由这里改（[`Machine::apply`]）：每次都经纯函数 [`transition`]。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rust_cli_base::{Task, TaskScope};
use rust_cli_exec_client::{ClientError, ClientOptions, ConnectionState, ExecClient};
use rust_cli_exec_transport::{Connector, LinkError, ProvisionStep, Provisioned, Provisioner};
use tokio::sync::watch;

use crate::state::{MachineError, MachineEvent, MachineState, Retry, Welcomed, transition};

/// 一台机器怎么到达（组装根注入；SSH 的实现在 cli，用 ssh-connect）。
pub trait Endpoint: Send + Sync {
    /// 日志里的名字。
    fn describe(&self) -> String;

    /// 让目标上有可用的执行器。
    fn provisioner(&self) -> &dyn Provisioner;

    /// 部署给的启动命令 → 连接器（第一次连接与之后的重连都经它）。
    fn connector(&self, provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError>;
}

/// 一台机器。
pub struct Machine {
    endpoint: Arc<dyn Endpoint>,
    options: ClientOptions,
    state: Arc<watch::Sender<MachineState>>,
    client: Mutex<Option<ExecClient>>,
    watcher: Mutex<Option<Task<()>>>,
    scope: TaskScope,
    /// 建过几个客户端：每个新的客户端一个实例（见 `connect`）。
    clients: AtomicU64,
}

fn welcomed(client: &ExecClient) -> Option<Welcomed> {
    client.welcome().map(|welcome| Welcomed {
        server_version: welcome.server_version,
        server_instance: welcome.server_instance,
        os: welcome.os,
        arch: welcome.arch,
    })
}

/// 按事件改状态；不合法的组合记日志、不改。
fn apply(state: &watch::Sender<MachineState>, name: &str, event: MachineEvent) -> bool {
    let shown = format!("{event:?}");
    state.send_if_modified(|current| match transition(current, event) {
        Some(next) => {
            tracing::info!(target: "machines", machine = %name, from = ?current, to = ?next, "machine state");
            let changed = *current != next;
            *current = next;
            changed
        }
        None => {
            tracing::debug!(target: "machines", machine = %name, state = ?current, event = %shown, "ignored machine event");
            false
        }
    })
}

impl Machine {
    pub fn new(endpoint: Arc<dyn Endpoint>, options: ClientOptions) -> Self {
        let scope = TaskScope::root("machines", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "machine task panicked");
        });
        Self {
            endpoint,
            options,
            state: Arc::new(watch::channel(MachineState::Unconnected).0),
            client: Mutex::new(None),
            watcher: Mutex::new(None),
            scope,
            clients: AtomicU64::new(0),
        }
    }

    /// 新客户端的参数：第一个用组装根给的实例，之后每个加一个序号。
    ///
    /// 缺陷修复（R5 发现）：以前断开或失败之后再连，新的 ExecClient 沿用同一个实例，调用 ID 又从 1 起；执行器那边还
    /// 留着旧客户端没 ack 的调用与流（账本的键是（实例，调用 ID）），新的调用撞上它们，回 `operation_conflict` 或接上
    /// 一个无关的旧流。同一个客户端之内的重连（Reconnecting）不经这里，实例不变（§18.5 的补发靠它）。
    fn next_options(&self) -> ClientOptions {
        let mut options = self.options.clone();
        let count = self.clients.fetch_add(1, Ordering::Relaxed);
        if count > 0 {
            options.client_instance = format!("{}.{count}", options.client_instance);
        }
        options
    }

    /// 订阅状态。
    pub fn states(&self) -> watch::Receiver<MachineState> {
        self.state.subscribe()
    }

    /// 现在的状态。
    pub fn state(&self) -> MachineState {
        self.state.borrow().clone()
    }

    fn apply(&self, event: MachineEvent) -> bool {
        apply(&self.state, &self.endpoint.describe(), event)
    }

    fn fail(&self, event: fn(MachineError) -> MachineEvent, error: MachineError) -> MachineError {
        self.apply(event(error.clone()));
        error
    }

    /// 部署、握手，成功后返回到执行器的客户端（之后的断线由客户端重连，状态跟着变）。已经连着时返回现有的客户端。
    pub async fn connect(&self) -> Result<ExecClient, MachineError> {
        // 重连次数用完（Failed）之后旧的客户端已经关了：丢掉它，重新部署、握手。
        if matches!(self.state(), MachineState::Failed { .. }) {
            lock(&self.client).take();
            if let Some(watcher) = lock(&self.watcher).take() {
                watcher.abort();
            }
        }
        if let Some(client) = self.client() {
            return Ok(client);
        }
        if !self.apply(MachineEvent::Connect) {
            return Err(MachineError::Lost(format!(
                "cannot connect while {:?}",
                self.state()
            )));
        }
        // 上传是单独的一步（F98，remote-exec.md §25.4.2）：部署器开始上传时报大小。
        let uploading = |step: ProvisionStep| match step {
            ProvisionStep::Uploading { bytes } => {
                self.apply(MachineEvent::Uploading { bytes });
            }
        };
        let provisioned = match self
            .endpoint
            .provisioner()
            .provision_reporting(&uploading)
            .await
        {
            Ok(provisioned) => provisioned,
            Err(error) => return Err(self.fail(MachineEvent::ProvisionFailed, error.into())),
        };
        self.apply(MachineEvent::Provisioned);
        let connector = self.endpoint.connector(&provisioned).map_err(|error| {
            self.fail(
                MachineEvent::HandshakeFailed,
                MachineError::Unreachable(error.to_string()),
            )
        })?;
        let client = ExecClient::connect(connector, self.next_options())
            .await
            .map_err(|error| self.fail(MachineEvent::HandshakeFailed, error.into()))?;
        let Some(first) = welcomed(&client) else {
            return Err(self.fail(
                MachineEvent::HandshakeFailed,
                MachineError::Lost("no welcome".to_owned()),
            ));
        };
        self.apply(MachineEvent::Welcomed(first));
        let watcher = self.scope.spawner().spawn(
            "watch",
            follow(client.clone(), self.state.clone(), self.endpoint.describe()),
        );
        *lock(&self.watcher) = Some(watcher);
        *lock(&self.client) = Some(client.clone());
        Ok(client)
    }

    /// 现在的客户端（没连着时 None）。
    pub fn client(&self) -> Option<ExecClient> {
        lock(&self.client).clone()
    }

    /// 立即重试（F98，remote-exec.md §25.4.1）：连接断了、在等下一次重连时马上开始下一次；没在等时不做事、回 false。
    pub fn retry_now(&self) -> bool {
        self.client().is_some_and(|client| client.retry_now())
    }

    /// 断开：关掉客户端，回到 Unconnected。
    pub async fn disconnect(&self) {
        if let Some(watcher) = lock(&self.watcher).take() {
            watcher.abort();
        }
        let client = lock(&self.client).take();
        if let Some(client) = client {
            client.close().await;
        }
        self.apply(MachineEvent::Disconnect);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 把 exec-client 的连接状态换成机器的事件。
async fn follow(client: ExecClient, state: Arc<watch::Sender<MachineState>>, name: String) {
    let mut states = client.states();
    states.borrow_and_update();
    while states.changed().await.is_ok() {
        let current = states.borrow_and_update().clone();
        let event = match current {
            // watch 只留最新值：先补上“断开”（已经在重连时不变），再记进度（第几次、下一次的时刻、原因，F96）。
            ConnectionState::Reconnecting {
                attempt,
                reason,
                retry_at,
            } => {
                apply(&state, &name, MachineEvent::LinkLost);
                MachineEvent::Retrying(Retry {
                    attempt,
                    retry_at,
                    reason,
                })
            }
            ConnectionState::Connected { .. } => match welcomed(&client) {
                Some(welcomed) => MachineEvent::Reattached(welcomed),
                None => continue,
            },
            // 自己关掉的客户端由 disconnect 回到 Unconnected。
            ConnectionState::Closed {
                error: ClientError::Closed,
            } => return,
            ConnectionState::Closed { error } => {
                apply(&state, &name, MachineEvent::GaveUp(error.into()));
                return;
            }
        };
        apply(&state, &name, event);
    }
}
