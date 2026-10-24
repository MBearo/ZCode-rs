//! 远端机器（docs/specs/rust-cli/remote-exec.md §20.5.1、§20.6）：发给模型的工具定义加 `machine` 参数，工具调用的缺省机器。
//!
//! 只在有远端机器连上时加；没有时工具定义逐字节不变。缺省机器：主线程是本机；子代理是派生时 Agent 的 `machine`
//! （`RunSink::default_machine`），它断开时是一台“没连上”的机器（调用报错，不落回本机）。

use rust_cli_kernel_api::env_registry::{LOCAL_MACHINE, MachineSet};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::machine_param::{AGENT_TOOL, choices, takes_machine, with_machine};
use rust_cli_kernel_api::model::ToolDefinition;
use rust_cli_prompts::texts::{self, render};

use crate::RunScope;

/// 参数的描述：缺省是本机或别的机器；Agent 的另一种写法。
fn description(tool: &str, default: Option<&str>) -> String {
    let agent = tool == AGENT_TOOL;
    match default {
        None => if agent {
            texts::machine_param_agent()
        } else {
            texts::machine_param()
        }
        .to_owned(),
        Some(name) => render(
            if agent {
                texts::machine_param_agent_default()
            } else {
                texts::machine_param_default()
            },
            &[("name", name)],
        ),
    }
}

/// 给带 `machine` 参数的工具加上它（[`with_machine`]）。`default` 是这次运行的缺省机器（本机为 None）。
/// 没有远端机器连上时什么都不改。
pub(crate) fn add_machine_param(
    definitions: &mut [ToolDefinition],
    machines: &MachineSet,
    default: Option<&str>,
) {
    if machines.is_empty() {
        return;
    }
    let choices = choices(machines);
    for definition in definitions
        .iter_mut()
        .filter(|definition| takes_machine(&definition.name))
    {
        definition.input_schema = with_machine(
            &definition.input_schema,
            &choices,
            &description(&definition.name, default),
        );
    }
}

impl RunScope {
    /// 这次运行的缺省机器的名字（本机为 None）。
    pub(crate) async fn default_machine_name(&self) -> Option<String> {
        self.sink
            .default_machine()
            .await
            .filter(|name| name != LOCAL_MACHINE)
    }

    /// 工具调用上下文里的缺省机器：本机，或子代理的缺省机器（没连上时是“没连上”的机器）。
    pub(crate) async fn default_machine(&self) -> MachineEnv {
        match self.default_machine_name().await {
            None => self.parts.machine.clone(),
            Some(name) => self
                .parts
                .machines
                .machine(&name)
                .unwrap_or_else(|| MachineEnv::unavailable(&name)),
        }
    }
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
