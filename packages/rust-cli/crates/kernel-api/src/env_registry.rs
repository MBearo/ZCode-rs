//! 执行环境的注册表（docs/specs/rust-cli/remote-exec.md §15.8、§15.8.1、§20.4）：机器名 → [`MachineEnv`]，与本机的
//! [`LocalServices`]。
//!
//! 只在组装根建：入口建注册表，runtime 从它取本机的端口分给各能力。R3 起连上的远端机器也在这里：工具执行按调用的
//! `machine` 查它（[`EnvironmentRegistry::remote`]），各能力每次用时读 [`EnvironmentRegistry::machines`] 的快照，连接
//! 变化经 [`EnvironmentRegistry::changes`] 推给要订阅的地方。注册表是“哪些机器连着”的唯一来源。

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use futures_util::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::env::{EnvPath, WorkspaceRef};
use crate::env_files::PathStyle;
use crate::local_services::LocalServices;
use crate::machine::MachineEnv;

/// 本机的机器名。
pub const LOCAL_MACHINE: &str = "local";

/// 一台远端机器的事实（握手时执行器报告的，remote-exec.md §18.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineFacts {
    /// 系统（`linux`、`macos`……，执行器的写法）。
    pub os: String,
    /// 架构（`x86_64`、`aarch64`……）。
    pub arch: String,
    /// 执行器跑命令用的 shell（可能为空）。
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<EnvPath>,
    pub path_style: PathStyle,
}

/// 一台连上的远端机器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMachine {
    pub name: String,
    /// 那台机器上的工作区：`environment` 是机器名，`root` 是它的工作目录（remote-exec.md §20.3.1）。
    pub workspace: WorkspaceRef,
    pub facts: MachineFacts,
}

/// 连上的远端机器的快照（不含本机）。`version` 每次连接变化加一；机器按名字排。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSet {
    pub version: u64,
    pub machines: Vec<RemoteMachine>,
}

impl MachineSet {
    pub fn is_empty(&self) -> bool {
        self.machines.is_empty()
    }

    /// 连上的远端机器的名字（按名字）。
    pub fn names(&self) -> Vec<String> {
        self.machines
            .iter()
            .map(|machine| machine.name.clone())
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&RemoteMachine> {
        self.machines.iter().find(|machine| machine.name == name)
    }
}

pub trait EnvironmentRegistry: Send + Sync {
    /// 本机（[`LOCAL_MACHINE`]）：每个注册表都有。
    fn local_machine(&self) -> MachineEnv;

    /// 按名字取一台机器；没有这台机器（或没连上）时 None。缺省：本机或 [`EnvironmentRegistry::remote`]。
    fn machine(&self, name: &str) -> Option<MachineEnv> {
        if name == LOCAL_MACHINE {
            return Some(self.local_machine());
        }
        self.remote(name).map(|(machine, _)| machine)
    }

    /// 只在本机的服务。
    fn local(&self) -> LocalServices;

    /// 连上的远端机器（R3）：内存里的快照，读的时候不做 IO。缺省没有。
    fn machines(&self) -> MachineSet {
        MachineSet::default()
    }

    /// 一台连上的远端机器的执行端口与它的工作区；本机、不认识或没连上时 None。缺省没有。
    fn remote(&self, _name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
        None
    }

    /// 连接变化：每次 [`EnvironmentRegistry::machines`] 的 `version` 变了出一项（新的 version）。只用来唤醒订阅方，
    /// 订阅方醒来后自己读快照。缺省永不出项。
    fn changes(&self) -> BoxStream<'static, u64> {
        futures_util::stream::pending().boxed()
    }

    /// 同 [`EnvironmentRegistry::remote`]，但这台机器正在连（`/machines connect`、启动时的自动连接）时等它连完
    /// （R5：绑定到它的 MCP 服务器在握手的期限之内等，remote-exec.md §22.6）。不认识、没在连或连不上时 None。
    /// 缺省不等：立即返回 `remote(name)`。取消由调用方负责（丢掉这个 future）。
    fn wait_remote(&self, name: &str) -> BoxFuture<'static, Option<(MachineEnv, WorkspaceRef)>> {
        let found = self.remote(name);
        async move { found }.boxed()
    }

    /// 这台连上的远端机器的连接断了、正在重连（远端执行 F96，remote-exec.md §23.3）：它还在 [`Self::remote`] 里（显式的
    /// 调用等重连），隐式访问它的地方（后台任务的快照、每步的文件变化检查、会话关闭）不等它。不做 IO。缺省 false。
    fn reconnecting(&self, _name: &str) -> bool {
        false
    }

    /// 这台机器在机器清单里（连着或没连着，远端执行 F98，remote-exec.md §25.2.2）：模型点名一台清单里有、现在没连上
    /// 的机器时，执行管线回“没有执行”的说明，不报 schema 错误。读内存里的清单副本，不做 IO。缺省 false。
    fn listed(&self, _name: &str) -> bool {
        false
    }
}

/// R0 的注册表：只有本机。
#[derive(Clone)]
pub struct LocalRegistry {
    machine: MachineEnv,
    services: LocalServices,
}

impl LocalRegistry {
    pub fn new(machine: MachineEnv, services: LocalServices) -> Self {
        Self { machine, services }
    }
}

impl EnvironmentRegistry for LocalRegistry {
    fn local_machine(&self) -> MachineEnv {
        self.machine.clone()
    }

    fn local(&self) -> LocalServices {
        self.services.clone()
    }
}

#[cfg(test)]
#[path = "env_registry_tests.rs"]
mod tests;
