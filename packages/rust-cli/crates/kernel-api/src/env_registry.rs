//! 执行环境的注册表（docs/specs/rust-cli/remote-exec.md §15.8、§15.8.1）：机器名 → [`MachineEnv`]，与本机的
//! [`LocalServices`]。
//!
//! 只在组装根用：入口建注册表，runtime 从它取本机的端口分给各能力；内核能力 crate 不拿注册表。R0 只有本机
//! （[`LocalRegistry`]）；R3 起连上的远端机器也在这里，工具执行按调用的 `machine` 查它。

use crate::local_services::LocalServices;
use crate::machine::MachineEnv;

/// 本机的机器名。
pub const LOCAL_MACHINE: &str = "local";

pub trait EnvironmentRegistry: Send + Sync {
    /// 本机（[`LOCAL_MACHINE`]）：每个注册表都有。
    fn local_machine(&self) -> MachineEnv;

    /// 按名字取一台机器；没有这台机器时 None。缺省只认本机。
    fn machine(&self, name: &str) -> Option<MachineEnv> {
        (name == LOCAL_MACHINE).then(|| self.local_machine())
    }

    /// 只在本机的服务。
    fn local(&self) -> LocalServices;
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
