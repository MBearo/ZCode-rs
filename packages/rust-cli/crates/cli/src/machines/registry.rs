//! 有远端机器的注册表（docs/specs/rust-cli/remote-exec.md §20.2、§20.4）：本机的部分来自原来的注册表，连上的远端机器
//! 由连接任务放进来、断开时拿走。它是“哪些机器连着”的唯一来源；各能力每次用时读快照，`changes()` 只用来唤醒订阅方。
//!
//! R5（§22.6）另记“正在连”的机器：`Fleet::connect` 在跑时（含启动时的自动连接）拿着一个 [`Connecting`]；绑定到它的
//! MCP 服务器经 `wait_remote` 等它连完。等待由状态变化唤醒，不轮询。
//!
//! F96（§23.3）：“这台在不在重连”直接读放进来时交给它的 exec-client 连接状态（`watch`），不另存一份。
//!
//! F98（§25.2.2）：“这台在不在清单里”读 Fleet 内存里的清单（Fleet 建好时交给这里一个查询），也不另存一份。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use futures_util::{FutureExt, StreamExt};
use rust_cli_exec_client::ConnectionState;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, MachineSet, RemoteMachine};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use tokio::sync::watch;

struct Connected {
    machine: MachineEnv,
    info: RemoteMachine,
    /// 这台机器的 exec-client 的连接状态（F96：`reconnecting` 读它）。
    link: watch::Receiver<ConnectionState>,
}

#[derive(Default)]
struct State {
    version: u64,
    connected: BTreeMap<String, Connected>,
    /// 正在连的机器与在连的请求数（R5）。
    connecting: BTreeMap<String, usize>,
}

/// 状态与“连着或正在连变了”的通知（`wait_remote` 的 future 要拿着它们，不借注册表）。
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    settled: watch::Sender<u64>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn settle(&self) {
        self.settled.send_modify(|version| *version += 1);
    }
}

/// 机器清单的查询（Fleet 给的：读它内存里的清单副本）。
pub(crate) type Listing = Box<dyn Fn(&str) -> bool + Send + Sync>;

pub(crate) struct MachineRegistry {
    local: Arc<dyn EnvironmentRegistry>,
    shared: Arc<Shared>,
    changes: watch::Sender<u64>,
    /// 清单（F98）：没有 Fleet（release 的 `-p`）时没有，`listed` 恒为假。
    listing: OnceLock<Listing>,
}

/// 一台机器正在连：丢掉时（连完、失败或请求被取消）不再算。
pub(crate) struct Connecting {
    shared: Arc<Shared>,
    name: String,
}

impl Drop for Connecting {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state();
            if let Some(count) = state.connecting.get_mut(&self.name) {
                *count -= 1;
                if *count == 0 {
                    state.connecting.remove(&self.name);
                }
            }
        }
        self.shared.settle();
    }
}

impl MachineRegistry {
    pub(crate) fn new(local: Arc<dyn EnvironmentRegistry>) -> Self {
        Self {
            local,
            shared: Arc::default(),
            changes: watch::channel(0).0,
            listing: OnceLock::new(),
        }
    }

    /// Fleet 建好时交来清单的查询（只认第一次）。
    pub(crate) fn set_listing(&self, listing: Listing) {
        let _ = self.listing.set(listing);
    }

    /// 一台机器连上（或重连成功）：放进快照。`machine` 已经带了机器名；`link` 是它的 exec-client 的连接状态。
    pub(crate) fn connected(
        &self,
        machine: MachineEnv,
        info: RemoteMachine,
        link: watch::Receiver<ConnectionState>,
    ) {
        let version = {
            let mut state = self.shared.state();
            state.version += 1;
            state.connected.insert(
                info.name.clone(),
                Connected {
                    machine,
                    info,
                    link,
                },
            );
            state.version
        };
        self.changes.send_replace(version);
        self.shared.settle();
    }

    /// 一台机器断开：从快照里拿走。不在快照里时什么都不做。
    pub(crate) fn disconnected(&self, name: &str) {
        let version = {
            let mut state = self.shared.state();
            if state.connected.remove(name).is_none() {
                return;
            }
            state.version += 1;
            state.version
        };
        self.changes.send_replace(version);
        self.shared.settle();
    }

    /// 记下这台机器正在连（R5，§22.6），直到返回的凭证被丢掉。
    pub(crate) fn connecting(&self, name: &str) -> Connecting {
        *self
            .shared
            .state()
            .connecting
            .entry(name.to_owned())
            .or_default() += 1;
        self.shared.settle();
        Connecting {
            shared: self.shared.clone(),
            name: name.to_owned(),
        }
    }
}

fn lookup(state: &State, name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
    state
        .connected
        .get(name)
        .map(|c| (c.machine.clone(), c.info.workspace.clone()))
}

impl EnvironmentRegistry for MachineRegistry {
    fn local_machine(&self) -> MachineEnv {
        self.local.local_machine()
    }

    fn local(&self) -> LocalServices {
        self.local.local()
    }

    fn machines(&self) -> MachineSet {
        let state = self.shared.state();
        MachineSet {
            version: state.version,
            machines: state.connected.values().map(|c| c.info.clone()).collect(),
        }
    }

    fn remote(&self, name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
        lookup(&self.shared.state(), name)
    }

    fn reconnecting(&self, name: &str) -> bool {
        self.shared
            .state()
            .connected
            .get(name)
            .is_some_and(|c| matches!(*c.link.borrow(), ConnectionState::Reconnecting { .. }))
    }

    fn listed(&self, name: &str) -> bool {
        self.listing.get().is_some_and(|listing| listing(name))
    }

    fn changes(&self) -> BoxStream<'static, u64> {
        let receiver = self.changes.subscribe();
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            let version = *receiver.borrow_and_update();
            Some((version, receiver))
        })
        .boxed()
    }

    fn wait_remote(&self, name: &str) -> BoxFuture<'static, Option<(MachineEnv, WorkspaceRef)>> {
        let shared = self.shared.clone();
        let name = name.to_owned();
        async move {
            // 先订阅再看状态：看完之后的变化都会唤醒。
            let mut settled = shared.settled.subscribe();
            loop {
                {
                    let state = shared.state();
                    if let Some(found) = lookup(&state, &name) {
                        return Some(found);
                    }
                    if !state.connecting.contains_key(&name) {
                        return None;
                    }
                }
                settled.changed().await.ok()?;
            }
        }
        .boxed()
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
