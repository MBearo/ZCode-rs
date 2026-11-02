//! 远端机器（docs/specs/rust-cli/remote-exec.md §20.5.1、§20.6）：发给模型的工具定义加 `machine` 参数，工具调用的缺省机器。
//!
//! 只在有远端机器连上时加；没有时工具定义逐字节不变。缺省机器：主线程是本机；子代理是派生时 Agent 的 `machine`
//! （`QueryRequest::default_machine`），它断开时是一台“没连上”的机器（调用报错，不落回本机）。

use std::collections::HashMap;
use std::sync::Arc;

use rust_cli_kernel_api::env_registry::MachineSet;
use rust_cli_kernel_api::ids::MachineName;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::machine_param::{MachineParam, choices, with_machine};
use rust_cli_kernel_api::model::ToolDefinition;
use rust_cli_kernel_api::tool::Tool;
use rust_cli_prompts::texts::{self, render};

use crate::RunScope;

/// 参数的描述：写出缺省机器的名字（主线程是主 env，子代理是它的缺省机器，deployment.md §23）；交给那台机器的工具
/// （Agent）是另一种写法。不带参数的工具没有描述。
fn description(param: MachineParam, default: &MachineName) -> Option<String> {
    let template = match param {
        MachineParam::Absent => return None,
        MachineParam::RunsThere => texts::machine_param(),
        MachineParam::Delegates => texts::machine_param_agent(),
    };
    Some(render(template, &[("name", default.as_str())]))
}

/// 给带 `machine` 参数的工具加上它（[`with_machine`]）。`tools` 是这一步的工具（定义出自它们，按名字对上）；
/// `default` 是这次运行的缺省机器。没有远端机器连上时什么都不改。
pub(crate) fn add_machine_param(
    definitions: &mut [ToolDefinition],
    tools: &[Arc<dyn Tool>],
    machines: &MachineSet,
    default: &MachineName,
) {
    if machines.is_empty() {
        return;
    }
    let params: HashMap<&str, MachineParam> = tools
        .iter()
        .map(|tool| (tool.name(), tool.machine_param()))
        .collect();
    let choices = choices(machines);
    for definition in definitions.iter_mut() {
        let param = params
            .get(definition.name.as_str())
            .copied()
            .unwrap_or(MachineParam::Absent);
        if let Some(description) = description(param, default) {
            definition.input_schema =
                with_machine(&definition.input_schema, &choices, &description);
        }
    }
}

impl RunScope {
    /// 工具调用上下文里的缺省机器：主 env，或子代理的缺省机器（没连上时是“没连上”的机器）。
    pub(crate) fn default_machine_env(&self) -> MachineEnv {
        let name = &self.default_machine;
        self.parts
            .machines
            .machine(name)
            .unwrap_or_else(|| MachineEnv::unavailable(name.as_str()))
    }
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
