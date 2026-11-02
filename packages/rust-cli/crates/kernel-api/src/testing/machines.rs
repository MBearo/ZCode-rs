//! 有远端机器的注册表替身（remote-exec.md §20.4）：本机加几台“远端”（各是一个替身，带名字与工作区），测试中途可以
//! 连上、断开；`changes()` 随之出项。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::watch;

use super::{AllPorts, machine};
use crate::controller_services::ControllerServices;
use crate::env::{EnvPath, WorkspaceRef};
use crate::env_files::PathStyle;
use crate::env_registry::{EnvironmentRegistry, MachineFacts, MachineSet, RemoteMachine};
use crate::ids::{EnvironmentId, MachineName};
use crate::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Git, MachineEnv, Pdf, Search, Shell,
};

struct Connected {
    machine: MachineEnv,
    info: RemoteMachine,
}

/// 主 env 与可以连上、断开的额外机器。
pub struct TestMachines {
    primary: MachineEnv,
    controller: ControllerServices,
    connected: Mutex<(u64, BTreeMap<String, Connected>)>,
    changes: watch::Sender<u64>,
    /// 正在重连的机器（F96，remote-exec.md §23.3）：还在 `remote()` 里。
    reconnecting: Mutex<BTreeSet<String>>,
    /// 机器清单里的机器（F98，remote-exec.md §25.2.2）：连上过的与 [`TestMachines::list`] 登记的；断开后仍在。
    listed: Mutex<BTreeSet<String>>,
}

impl TestMachines {
    pub fn new<E: AllPorts + 'static>(primary: Arc<E>) -> Arc<Self> {
        Self::build(machine(primary.clone()), ControllerServices::new(primary))
    }

    /// 同 [`TestMachines::new`]，主 env 已按端口装好（`unsupported_machine().with_*`，F126）；controller 的服务都返回
    /// `Unsupported`。
    pub fn with_primary(primary: MachineEnv) -> Arc<Self> {
        Self::build(primary, super::unsupported_controller())
    }

    fn build(primary: MachineEnv, controller: ControllerServices) -> Arc<Self> {
        Arc::new(Self {
            primary,
            controller,
            connected: Mutex::new((0, BTreeMap::new())),
            changes: watch::channel(0).0,
            reconnecting: Mutex::default(),
            listed: Mutex::default(),
        })
    }

    /// 连上一台叫 `name` 的机器：执行端口是 `env`，工作目录是 `root`（Linux x86_64、`/bin/bash`）。
    pub fn connect<E>(&self, name: &str, env: Arc<E>, root: &str)
    where
        E: EnvInfo
            + Files
            + Search
            + Commands
            + Shell
            + Background
            + Checkpoints
            + Pdf
            + Git
            + 'static,
    {
        self.connect_machine(name, machine(env), root);
    }

    /// 同 [`TestMachines::connect`]，机器已按端口装好（`unsupported_machine().with_*`，F126）。
    pub fn connect_machine(&self, name: &str, machine: MachineEnv, root: &str) {
        let info = RemoteMachine {
            name: name.to_owned(),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new(name),
                root: EnvPath::new(root),
            },
            facts: MachineFacts {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                shell: "/bin/bash".to_owned(),
                home: Some(EnvPath::new("/home/dev")),
                path_style: PathStyle::Posix,
            },
        };
        let version = {
            let mut connected = self.connected.lock_unpoisoned();
            connected.0 += 1;
            connected.1.insert(
                name.to_owned(),
                Connected {
                    machine: machine.named(crate::ids::MachineName::new(name)),
                    info,
                },
            );
            connected.0
        };
        self.list(name);
        self.changes.send_replace(version);
    }

    /// 只登记到机器清单、不连（F98：清单里有、没连上的机器）。
    pub fn list(&self, name: &str) {
        self.listed.lock_unpoisoned().insert(name.to_owned());
    }

    /// 断开一台机器。
    pub fn disconnect(&self, name: &str) {
        let version = {
            let mut connected = self.connected.lock_unpoisoned();
            connected.0 += 1;
            connected.1.remove(name);
            connected.0
        };
        self.reconnecting.lock_unpoisoned().remove(name);
        self.changes.send_replace(version);
    }

    /// 这台机器开始或结束重连（F96）：快照的 version 不变（同 cli 的注册表）。
    pub fn set_reconnecting(&self, name: &str, reconnecting: bool) {
        let mut set = self.reconnecting.lock_unpoisoned();
        if reconnecting {
            set.insert(name.to_owned());
        } else {
            set.remove(name);
        }
    }
}

impl EnvironmentRegistry for TestMachines {
    fn primary_machine(&self) -> MachineEnv {
        self.primary.clone()
    }

    fn controller(&self) -> ControllerServices {
        self.controller.clone()
    }

    fn machines(&self) -> MachineSet {
        let connected = self.connected.lock_unpoisoned();
        MachineSet {
            version: connected.0,
            machines: connected.1.values().map(|c| c.info.clone()).collect(),
        }
    }

    fn remote(&self, name: &MachineName) -> Option<(MachineEnv, WorkspaceRef)> {
        let connected = self.connected.lock_unpoisoned();
        connected
            .1
            .get(name.remote()?)
            .map(|c| (c.machine.clone(), c.info.workspace.clone()))
    }

    fn reconnecting(&self, name: &MachineName) -> bool {
        name.remote()
            .is_some_and(|name| self.reconnecting.lock_unpoisoned().contains(name))
    }

    fn listed(&self, name: &MachineName) -> bool {
        name.remote()
            .is_some_and(|name| self.listed.lock_unpoisoned().contains(name))
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
