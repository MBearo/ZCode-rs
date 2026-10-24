//! 有远端机器的注册表（docs/specs/rust-cli/remote-exec.md §20.2、§20.4）：本机的部分来自原来的注册表，连上的远端机器
//! 由连接任务放进来、断开时拿走。它是“哪些机器连着”的唯一来源；各能力每次用时读快照，`changes()` 只用来唤醒订阅方。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, MachineSet, RemoteMachine};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use tokio::sync::watch;

struct Connected {
    machine: MachineEnv,
    info: RemoteMachine,
}

#[derive(Default)]
struct State {
    version: u64,
    connected: BTreeMap<String, Connected>,
}

pub(crate) struct MachineRegistry {
    local: Arc<dyn EnvironmentRegistry>,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
}

impl MachineRegistry {
    pub(crate) fn new(local: Arc<dyn EnvironmentRegistry>) -> Self {
        Self {
            local,
            state: Mutex::new(State::default()),
            changes: watch::channel(0).0,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 一台机器连上（或重连成功）：放进快照。`machine` 已经带了机器名。
    pub(crate) fn connected(&self, machine: MachineEnv, info: RemoteMachine) {
        let version = {
            let mut state = self.state();
            state.version += 1;
            state
                .connected
                .insert(info.name.clone(), Connected { machine, info });
            state.version
        };
        self.changes.send_replace(version);
    }

    /// 一台机器断开：从快照里拿走。不在快照里时什么都不做。
    pub(crate) fn disconnected(&self, name: &str) {
        let version = {
            let mut state = self.state();
            if state.connected.remove(name).is_none() {
                return;
            }
            state.version += 1;
            state.version
        };
        self.changes.send_replace(version);
    }
}

impl EnvironmentRegistry for MachineRegistry {
    fn local_machine(&self) -> MachineEnv {
        self.local.local_machine()
    }

    fn local(&self) -> LocalServices {
        self.local.local()
    }

    fn machines(&self) -> MachineSet {
        let state = self.state();
        MachineSet {
            version: state.version,
            machines: state.connected.values().map(|c| c.info.clone()).collect(),
        }
    }

    fn remote(&self, name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
        self.state()
            .connected
            .get(name)
            .map(|c| (c.machine.clone(), c.info.workspace.clone()))
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
}
