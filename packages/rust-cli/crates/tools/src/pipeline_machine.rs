//! 执行管线里的机器路由（docs/specs/rust-cli/remote-exec.md §20.6）：按调用的 `machine` 选机器。
//!
//! - 带 `machine` 参数的工具：有远端机器连上时，schema 校验用加了参数的 schema（不认识的机器名由 `enum` 报错）；
//!   取出并去掉 `machine`，没写就是缺省机器（主线程本机，子代理派生时给的机器）。远端调用的上下文换成那台机器与它的
//!   工作区（Agent 只换机器：它自己在本机跑，工作区不换）。
//! - 不带参数的工具：上下文的机器换回本机（子代理在远端时，WebFetch、TaskStop、EnterWorktree 仍在本机）。
//! - hooks 用会话的上下文（本机的工作区与 cwd），payload 带 `machine`。
//!
//! 没有注册表（测试）或没有远端机器、缺省也是本机时，什么都不做：上下文、schema 与 hooks 的输入都不变。

use std::sync::Arc;

use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, MachineSet};
use rust_cli_kernel_api::ids::{LEGACY_PRIMARY_MACHINE, MachineName, PRIMARY_MACHINE};
use rust_cli_kernel_api::machine_param::{
    MACHINE, MachineParam, choices, named_in, take, with_machine,
};
use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
use rust_cli_prompts::texts;
use serde_json::Value;

/// 一次调用的两份上下文：工具与判决链用路由后的；hooks 用会话的。
pub(crate) struct Routed {
    pub context: ToolUseContext,
    pub hooks: ToolUseContext,
    /// 调用的机器（hooks 的 `machine`、询问的 `machine` 只给额外机器）。
    pub machine: MachineName,
}

impl Routed {
    /// 不换机器：两份都是原来的上下文。
    pub(crate) fn unchanged(context: ToolUseContext) -> Self {
        Self {
            hooks: context.clone(),
            context,
            machine: MachineName::primary(),
        }
    }
}

/// 这次调用怎么路由。
pub(crate) enum Plan {
    /// 不路由（没有远端机器、缺省是本机，或没有注册表）。
    Unchanged,
    /// 不带参数的工具，但上下文的机器是远端：换回本机。
    ToLocal,
    /// 带参数的工具：按 `machine` 选；`machines` 是这时连着的远端机器。
    Route { machines: MachineSet },
}

/// 管线持有的注册表。
#[derive(Clone, Default)]
pub(crate) struct Machines(Option<Arc<dyn EnvironmentRegistry>>);

impl Machines {
    pub(crate) fn new(registry: Arc<dyn EnvironmentRegistry>) -> Self {
        Self(Some(registry))
    }

    pub(crate) fn plan(&self, tool: &dyn Tool, context: &ToolUseContext) -> Plan {
        let Some(registry) = &self.0 else {
            return Plan::Unchanged;
        };
        if tool.machine_param() == MachineParam::Absent {
            return if context.machine.is_primary() {
                Plan::Unchanged
            } else {
                Plan::ToLocal
            };
        }
        let machines = registry.machines();
        if machines.is_empty() && context.machine.is_primary() {
            return Plan::Unchanged;
        }
        Plan::Route { machines }
    }

    /// 输入点名的是清单里有、现在没连上的机器：给模型的“没有执行”说明（远端执行 F98，remote-exec.md §25.2.2）。
    ///
    /// 缺陷修复（Docker 试用 M2）：schema 校验在路由之前，没连上的机器不在工具定义里（没有远端机器时没有 `machine`，
    /// 有别的机器时 `enum` 里没有它），模型拿到的是 schema 错误，F96 的“没有执行”说明到不了；宽松对象的 Agent 甚至丢掉
    /// `machine`、在本机起子代理。现在先于 schema 校验认出它。不在清单里的名字照旧交给 schema 校验。
    pub(crate) fn not_connected(&self, tool: &dyn Tool, input: &Value) -> Option<String> {
        let registry = self.0.as_ref()?;
        let name = named_in(input).filter(|_| tool.machine_param() != MachineParam::Absent)?;
        let machine = MachineName::new(name);
        (registry.remote(&machine).is_none() && registry.listed(&machine))
            .then(|| not_connected(name))
    }

    /// 校验用的 schema：有远端机器连上时加上 `machine`（与发给模型的相同的取值）；否则是工具自己的。
    pub(crate) fn schema(base: Value, plan: &Plan) -> Value {
        match plan {
            Plan::Route { machines } if !machines.is_empty() => {
                // 描述不参与校验。主 env 以前的名字（`local`）也认：旧会话里模型照着历史写的调用（deployment.md §23）。
                let mut values = choices(machines);
                values.push(LEGACY_PRIMARY_MACHINE.to_owned());
                let description =
                    texts::render(texts::machine_param(), &[("name", PRIMARY_MACHINE)]);
                with_machine(&base, &values, &description)
            }
            _ => base,
        }
    }

    /// 取出 `machine`、建两份上下文。机器没连上时是给模型的错误文字。
    pub(crate) fn route(
        &self,
        tool: &dyn Tool,
        input: &mut Value,
        context: ToolUseContext,
        plan: &Plan,
    ) -> Result<Routed, String> {
        let Some(registry) = &self.0 else {
            return Ok(Routed::unchanged(context));
        };
        match plan {
            Plan::Unchanged => Ok(Routed::unchanged(context)),
            Plan::ToLocal => {
                let mut routed = context.clone();
                routed.machine = registry.primary_machine();
                Ok(Routed {
                    context: routed,
                    hooks: context,
                    machine: MachineName::primary(),
                })
            }
            Plan::Route { machines } => {
                // schema 校验在前（加了参数时值一定是取值之一；没加参数时 `machine` 已被 schema 拒绝）。
                let named = if machines.is_empty() {
                    None
                } else {
                    take(input)
                };
                let target = match &named {
                    Some(Value::String(name)) => MachineName::new(name.as_str()),
                    Some(other) => {
                        return Err(format!("`{MACHINE}` must be a string, got {other}"));
                    }
                    None => context.machine.name().clone(),
                };
                let mut routed = context.clone();
                let Some(remote) = target.remote() else {
                    routed.machine = registry.primary_machine();
                    return Ok(Routed {
                        context: routed,
                        hooks: context,
                        machine: target,
                    });
                };
                // 没连上的机器：写明没有执行、下一步做什么（远端执行 F96，remote-exec.md §23.6）。
                let Some((machine, workspace)) = registry.remote(&target) else {
                    return Err(not_connected(remote));
                };
                routed.machine = machine;
                match tool.machine_param() {
                    MachineParam::RunsThere => routed.workspace = workspace,
                    // Agent 自己在主 env 上跑，只把机器交给子代理（工作区不换）；不带参数的工具不走这一支（`plan` 给的是
                    // Unchanged 或 ToLocal）。
                    MachineParam::Delegates | MachineParam::Absent => {}
                }
                Ok(Routed {
                    context: routed,
                    hooks: context,
                    machine: target,
                })
            }
        }
    }
}

/// hooks 的 payload 加上 `machine`（只有额外机器上的调用，§20.8）。
pub(crate) fn tag(mut payload: Value, machine: &MachineName) -> Value {
    if let (Some(name), Some(object)) = (machine.remote(), payload.as_object_mut()) {
        object.insert(MACHINE.to_owned(), Value::from(name));
    }
    payload
}

/// 没连上的额外机器：给模型的“没有执行”说明（F96，remote-exec.md §23.6），建议改在缺省机器上跑（deployment.md §23）。
fn not_connected(name: &str) -> String {
    texts::render(
        texts::remote_not_connected(),
        &[
            ("machine", name),
            ("instead", rust_cli_prompts::remote::instead(false)),
        ],
    )
}
