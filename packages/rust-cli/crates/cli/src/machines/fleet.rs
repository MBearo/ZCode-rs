//! 机器清单与每台机器的连接（docs/specs/rust-cli/remote-exec.md §21.2）：TUI 的 `/machines` 与状态行经它看、经它改。
//!
//! 唯一事实源不变：一台机器的连接状态归它的 `Machine`（R2 的状态机），延迟归 exec-client，“哪些机器连着”归
//! `MachineRegistry`（R3），清单归 `machines.json`。这里只按需拼快照（读的时候取，不另存）；清单的内存副本是本进程最后一次
//! 读或写文件的结果。每台用过的机器一个跟随任务：状态变了就照 R3 放进或拿走注册表，并唤醒订阅方（`changes`）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rust_cli_base::{Spawner, Task};
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::ExecClient;
use rust_cli_exec_wire::handshake::Welcome;
use rust_cli_kernel_api::mcp::McpConnect;
use rust_cli_machines::{
    Machine, MachineEntry, MachineError, MachineList, MachineState, MachinesError,
};
use tokio::sync::watch;

use super::{MachineRegistry, attach, remote_of};

/// 按清单的条目建一台机器的状态机（端点由组装根选：SSH 或只给测试的 loopback；单测里是内存的）。
pub(crate) type MakeMachine = Box<dyn Fn(&MachineEntry) -> anyhow::Result<Machine> + Send + Sync>;

/// 一个操作为什么没做成。
#[derive(Debug)]
pub(crate) enum FleetError {
    Connection(MachineError),
    List(MachinesError),
}

impl From<MachinesError> for FleetError {
    fn from(error: MachinesError) -> Self {
        Self::List(error)
    }
}

impl std::fmt::Display for FleetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(error) => error.fmt(formatter),
            Self::List(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for FleetError {}

/// 连接与断开的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Changed {
    Done,
    /// 本来就是那样（已经连着 / 没连着）。
    Already,
}

/// 快照的一台：清单里的条目与它现在的连接事实。
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub entry: MachineEntry,
    pub state: MachineState,
    /// 最近一次心跳的往返；就绪时才给。
    pub latency: Option<Duration>,
    /// 最近一次握手（本进程连上过才有）。
    pub welcome: Option<Welcome>,
}

/// 同一台机器同时只有一个连接请求在跑（`true`）：第二个等它结束再看状态（锁不跨 await，用 watch 等）。
type Connecting = watch::Sender<bool>;

/// 一台用过的机器：状态机、连接用的条目、在跑的连接请求、跟随任务。
struct Slot {
    machine: Arc<Machine>,
    entry: MachineEntry,
    connecting: Connecting,
    follow: Task<()>,
    latency: Option<Task<()>>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.follow.abort();
        if let Some(latency) = &self.latency {
            latency.abort();
        }
    }
}

#[derive(Default)]
struct State {
    entries: Vec<MachineEntry>,
    slots: BTreeMap<String, Slot>,
}

pub(crate) struct Fleet {
    list: MachineList,
    make: MakeMachine,
    registry: Arc<MachineRegistry>,
    spawner: Spawner,
    /// 注册表的 `listed` 也读这里的清单（F98），所以拿 Arc。
    state: Arc<Mutex<State>>,
    changes: watch::Sender<u64>,
    /// 每台机器上期望的 MCP 服务器（R5，remote-exec.md §22.5）：机器名 → 绑定到它的服务器（配置的原文）。
    mcp: BTreeMap<String, Vec<McpConnect>>,
}

/// 占着“正在连”的凭证：丢掉时（连完、出错或请求被取消）放开。
struct Turn(Connecting);

impl Turn {
    /// 没人在连时占住它。
    fn take(connecting: &Connecting) -> Option<Self> {
        connecting
            .send_if_modified(|busy| !std::mem::replace(busy, true))
            .then(|| Self(connecting.clone()))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

fn lock_state(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 唤醒订阅方（版本加一）。
fn bump(changes: &watch::Sender<u64>) {
    changes.send_modify(|version| *version += 1);
}

impl Fleet {
    pub(crate) fn new(
        (list, make): (MachineList, MakeMachine),
        entries: Vec<MachineEntry>,
        registry: Arc<MachineRegistry>,
        spawner: Spawner,
    ) -> Self {
        let state = Arc::new(Mutex::new(State {
            entries,
            slots: BTreeMap::new(),
        }));
        // 注册表答“这台在不在清单里”时读同一份清单（F98，remote-exec.md §25.2.2）。
        let listing = Arc::downgrade(&state);
        registry.set_listing(Box::new(move |name| {
            listing.upgrade().is_some_and(|state| {
                lock_state(&state)
                    .entries
                    .iter()
                    .any(|entry| entry.name == name)
            })
        }));
        Self {
            list,
            make,
            registry,
            spawner,
            state,
            changes: watch::channel(0).0,
            mcp: BTreeMap::new(),
        }
    }

    /// 每台机器上期望的 MCP 服务器（连上时发给执行器，§22.5）。
    #[must_use]
    pub(crate) fn with_mcp(mut self, mcp: BTreeMap<String, Vec<McpConnect>>) -> Self {
        self.mcp = mcp;
        self
    }

    /// 期望集合（§22.5）：执行器结束这台机器上多出来的 MCP 服务器（配置删了、改了的），相同的不动。失败只记日志：
    /// 多出来的服务器留到执行器按 §19.7 清掉。
    async fn declare_mcp(&self, entry: &MachineEntry, client: &ExecClient) {
        let servers = self.mcp.get(&entry.name).cloned().unwrap_or_default();
        let workspace = match remote_of(entry, client) {
            Ok(remote) => remote.workspace,
            Err(error) => {
                tracing::warn!(machine = %entry.name, error = %format!("{error:#}"), "MCP servers not declared");
                return;
            }
        };
        match RemoteEnvironment::new(client.clone())
            .declare_mcp(&workspace, servers)
            .await
        {
            Ok(closed) if closed.is_empty() => {}
            Ok(closed) => {
                tracing::info!(machine = %entry.name, ?closed, "MCP servers that are not configured any more were closed");
            }
            Err(error) => {
                tracing::warn!(machine = %entry.name, %error, "MCP servers not declared");
            }
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock_state(&self.state)
    }

    /// 订阅变化：清单、状态或延迟（按毫秒）变了就出一个新的版本号。
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// 清单里的机器（按清单的顺序）与它们现在的连接事实。不做 IO。
    pub(crate) fn snapshot(&self) -> Vec<Seen> {
        let state = self.state();
        state
            .entries
            .iter()
            .map(|entry| {
                let machine = state.slots.get(&entry.name).map(|slot| &slot.machine);
                let current = machine.map_or(MachineState::Unconnected, |m| m.state());
                let client = machine.and_then(|m| m.client());
                let latency = client
                    .as_ref()
                    .filter(|_| matches!(current, MachineState::Ready(_)))
                    .and_then(ExecClient::latency);
                Seen {
                    entry: entry.clone(),
                    state: current,
                    latency,
                    welcome: client.as_ref().and_then(ExecClient::welcome),
                }
            })
            .collect()
    }

    /// 重读清单文件（本进程改过清单之后，别的进程的改动也一起看到）。
    async fn reload(&self) -> Result<(), MachinesError> {
        let entries = self.list.load().await?;
        self.state().entries = entries;
        bump(&self.changes);
        Ok(())
    }

    /// 清单里的这台；内存里没有时重读一次文件（别的进程加的也认）。
    async fn entry(&self, name: &str) -> Result<MachineEntry, MachinesError> {
        let find = |fleet: &Self| {
            fleet
                .state()
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .cloned()
        };
        if let Some(entry) = find(self) {
            return Ok(entry);
        }
        self.reload().await?;
        find(self).ok_or_else(|| MachinesError::Missing(name.to_owned()))
    }

    /// 写进清单（`/machines add`），不连。
    pub(crate) async fn add(&self, entry: MachineEntry) -> Result<(), FleetError> {
        self.list.add(entry).await?;
        self.reload().await?;
        Ok(())
    }

    /// 这台机器的状态机（没有就按 R3 的端点建一个，并开始跟随它）。
    fn slot(&self, entry: &MachineEntry) -> anyhow::Result<(Arc<Machine>, Connecting)> {
        let mut state = self.state();
        if let Some(slot) = state.slots.get(&entry.name) {
            return Ok((slot.machine.clone(), slot.connecting.clone()));
        }
        let machine = Arc::new((self.make)(entry)?);
        let follow = self.spawner.spawn(
            "machine-follow",
            follow(
                machine.clone(),
                entry.clone(),
                self.registry.clone(),
                self.changes.clone(),
            ),
        );
        let connecting = watch::channel(false).0;
        state.slots.insert(
            entry.name.clone(),
            Slot {
                machine: machine.clone(),
                entry: entry.clone(),
                connecting: connecting.clone(),
                follow,
                latency: None,
            },
        );
        Ok((machine, connecting))
    }

    /// 连上（失败过的重新部署、握手），成功后放进注册表、清单记 `connected=true`（§20.3.1）。
    pub(crate) async fn connect(&self, name: &str) -> Result<Changed, FleetError> {
        // 正在连（R5，§22.6）：绑定到这台机器的 MCP 服务器等它连完。
        let _connecting = self.registry.connecting(name);
        let entry = self.entry(name).await?;
        let (machine, connecting) = self.slot(&entry).map_err(|error| {
            FleetError::Connection(MachineError::Unreachable(format!("{error:#}")))
        })?;
        // 先占住“正在连”；别人占着时等它结束，再按那时的状态决定（连上了就是“已经连着”，失败了自己再连）。
        let _turn = loop {
            if matches!(
                machine.state(),
                MachineState::Ready(_) | MachineState::Reconnecting { .. }
            ) {
                return Ok(Changed::Already);
            }
            if let Some(turn) = Turn::take(&connecting) {
                break turn;
            }
            let mut busy = connecting.subscribe();
            // 发送端在 Slot 里；Slot 被删掉（remove）时等待结束，下一轮按状态处理。
            let _ = busy.wait_for(|busy| !busy).await;
        };
        let client = machine.connect().await.map_err(FleetError::Connection)?;
        let slot_entry = self
            .state()
            .slots
            .get(name)
            .map_or_else(|| entry.clone(), |slot| slot.entry.clone());
        // 放进注册表之前先发期望集合：ext 的 `mcp.connect` 都在它之后（§22.5）。
        self.declare_mcp(&slot_entry, &client).await;
        attach(&self.registry, &slot_entry, client.clone())
            .map_err(|error| FleetError::Connection(MachineError::Lost(format!("{error:#}"))))?;
        // 延迟：这个客户端的心跳往返（重连时是同一个客户端；失败后再连换新客户端，这里换掉旧的跟随）。
        let latency = self.spawner.spawn(
            "machine-latency",
            latency(client.latencies(), self.changes.clone()),
        );
        if let Some(slot) = self.state().slots.get_mut(name) {
            if let Some(old) = slot.latency.replace(latency) {
                old.abort();
            }
        } else {
            latency.abort();
        }
        if let Err(error) = self.list.set_connected(name, true).await {
            tracing::warn!(machine = %name, %error, "the machine list was not updated");
        }
        bump(&self.changes);
        Ok(Changed::Done)
    }

    /// 断开；清单记 `connected=false`（下次启动不自动连）。
    pub(crate) async fn disconnect(&self, name: &str) -> Result<Changed, FleetError> {
        self.entry(name).await?;
        let machine = self
            .state()
            .slots
            .get(name)
            .map(|slot| slot.machine.clone());
        let changed = match machine {
            Some(machine) if machine.state() != MachineState::Unconnected => {
                let live = !matches!(machine.state(), MachineState::Failed { .. });
                machine.disconnect().await;
                self.registry.disconnected(name);
                if live {
                    Changed::Done
                } else {
                    Changed::Already
                }
            }
            _ => Changed::Already,
        };
        self.list.set_connected(name, false).await?;
        bump(&self.changes);
        Ok(changed)
    }

    /// 立即重试（F98，remote-exec.md §25.4.1）：这台机器在等下一次重连时马上试一次，不等退避；没在等（正在连、连着、
    /// 没断开）时是 `Already`。重试的结果照常经状态机与 `changes` 显示。
    pub(crate) async fn retry(&self, name: &str) -> Result<Changed, FleetError> {
        self.entry(name).await?;
        let machine = self
            .state()
            .slots
            .get(name)
            .map(|slot| slot.machine.clone());
        Ok(if machine.is_some_and(|machine| machine.retry_now()) {
            Changed::Done
        } else {
            Changed::Already
        })
    }

    /// 连着时先断开，再从清单删掉。
    pub(crate) async fn remove(&self, name: &str) -> Result<(), FleetError> {
        self.entry(name).await?;
        let slot = self.state().slots.remove(name);
        if let Some(slot) = slot {
            slot.machine.disconnect().await;
            self.registry.disconnected(name);
        }
        self.list.remove(name).await?;
        self.reload().await?;
        Ok(())
    }

    /// 清单里 `connected` 为真的机器（启动时自动连，§20.3.2）。
    pub(crate) fn remembered(&self) -> Vec<String> {
        self.state()
            .entries
            .iter()
            .filter(|entry| entry.connected)
            .map(|entry| entry.name.clone())
            .collect()
    }
}

/// 跟随一台机器的状态（§20.3.2、§21.2）：重连成功时放回注册表（同一个客户端，attach 不重复放），重连失败或断开时拿走；
/// 每次变化都唤醒订阅方。第一次连上由 `Fleet::connect` 放进注册表（那时客户端才交出来）。
async fn follow(
    machine: Arc<Machine>,
    entry: MachineEntry,
    registry: Arc<MachineRegistry>,
    changes: watch::Sender<u64>,
) {
    let mut states = machine.states();
    states.borrow_and_update();
    while states.changed().await.is_ok() {
        let state = states.borrow_and_update().clone();
        match &state {
            MachineState::Ready(_) => {
                if let Some(client) = machine.client()
                    && let Err(error) = attach(&registry, &entry, client)
                {
                    tracing::warn!(machine = %entry.name, error = %format!("{error:#}"), "machine not attached");
                }
            }
            MachineState::Failed { .. } | MachineState::Unconnected => {
                registry.disconnected(&entry.name);
                tracing::info!(event = "machine.disconnected", machine = %entry.name, state = ?state, "machine disconnected");
            }
            MachineState::Provisioning
            | MachineState::Uploading { .. }
            | MachineState::Handshaking
            | MachineState::Reconnecting { .. } => {}
        }
        bump(&changes);
    }
}

/// 跟随一个客户端的延迟：按毫秒变了才唤醒（每 10 s 一次心跳）。只拿接收端：客户端没了它就结束。
async fn latency(mut latencies: watch::Receiver<Option<Duration>>, changes: watch::Sender<u64>) {
    let mut shown = None;
    while latencies.changed().await.is_ok() {
        let ms = latencies.borrow_and_update().map(|value| value.as_millis());
        if ms != shown {
            shown = ms;
            bump(&changes);
        }
    }
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "fleet_mcp_tests.rs"]
mod mcp_tests;
