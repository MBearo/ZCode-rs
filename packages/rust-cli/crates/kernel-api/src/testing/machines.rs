//! 有远端机器的注册表替身（remote-exec.md §20.4）：本机加几台“远端”（各是一个替身，带名字与工作区），测试中途可以
//! 连上、断开；`changes()` 随之出项。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::watch;

use super::{AllPorts, lock, machine};
use crate::env::{EnvPath, WorkspaceRef};
use crate::env_files::PathStyle;
use crate::env_registry::{EnvironmentRegistry, MachineFacts, MachineSet, RemoteMachine};
use crate::ids::EnvironmentId;
use crate::local_services::LocalServices;
use crate::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, MachineEnv, Pdf, Search, Shell,
};

struct Connected {
    machine: MachineEnv,
    info: RemoteMachine,
}

/// 本机与可以连上、断开的“远端”机器。
pub struct TestMachines {
    local: MachineEnv,
    services: LocalServices,
    connected: Mutex<(u64, BTreeMap<String, Connected>)>,
    changes: watch::Sender<u64>,
    /// 正在重连的机器（F96，remote-exec.md §23.3）：还在 `remote()` 里。
    reconnecting: Mutex<BTreeSet<String>>,
    /// 机器清单里的机器（F98，remote-exec.md §25.2.2）：连上过的与 [`TestMachines::list`] 登记的；断开后仍在。
    listed: Mutex<BTreeSet<String>>,
}

impl TestMachines {
    pub fn new<E: AllPorts + 'static>(local: Arc<E>) -> Arc<Self> {
        Arc::new(Self {
            local: machine(local.clone()),
            services: LocalServices::new(local),
            connected: Mutex::new((0, BTreeMap::new())),
            changes: watch::channel(0).0,
            reconnecting: Mutex::default(),
            listed: Mutex::default(),
        })
    }

    /// 连上一台叫 `name` 的机器：执行端口是 `env`，工作目录是 `root`（Linux x86_64、`/bin/bash`）。
    pub fn connect<E>(&self, name: &str, env: Arc<E>, root: &str)
    where
        E: EnvInfo + Files + Search + Commands + Shell + Background + Checkpoints + Pdf + 'static,
    {
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
            let mut connected = lock(&self.connected);
            connected.0 += 1;
            connected.1.insert(
                name.to_owned(),
                Connected {
                    machine: machine(env).named(name),
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
        lock(&self.listed).insert(name.to_owned());
    }

    /// 断开一台机器。
    pub fn disconnect(&self, name: &str) {
        let version = {
            let mut connected = lock(&self.connected);
            connected.0 += 1;
            connected.1.remove(name);
            connected.0
        };
        lock(&self.reconnecting).remove(name);
        self.changes.send_replace(version);
    }

    /// 这台机器开始或结束重连（F96）：快照的 version 不变（同 cli 的注册表）。
    pub fn set_reconnecting(&self, name: &str, reconnecting: bool) {
        let mut set = lock(&self.reconnecting);
        if reconnecting {
            set.insert(name.to_owned());
        } else {
            set.remove(name);
        }
    }
}

impl EnvironmentRegistry for TestMachines {
    fn local_machine(&self) -> MachineEnv {
        self.local.clone()
    }

    fn local(&self) -> LocalServices {
        self.services.clone()
    }

    fn machines(&self) -> MachineSet {
        let connected = lock(&self.connected);
        MachineSet {
            version: connected.0,
            machines: connected.1.values().map(|c| c.info.clone()).collect(),
        }
    }

    fn remote(&self, name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
        let connected = lock(&self.connected);
        connected
            .1
            .get(name)
            .map(|c| (c.machine.clone(), c.info.workspace.clone()))
    }

    fn reconnecting(&self, name: &str) -> bool {
        lock(&self.reconnecting).contains(name)
    }

    fn listed(&self, name: &str) -> bool {
        lock(&self.listed).contains(name)
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
