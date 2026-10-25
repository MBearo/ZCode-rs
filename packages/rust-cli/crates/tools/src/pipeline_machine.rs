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

use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, LOCAL_MACHINE, MachineSet};
use rust_cli_kernel_api::machine_param::{
    AGENT_TOOL, MACHINE, choices, named_in, take, takes_machine, with_machine,
};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_prompts::texts;
use serde_json::Value;

/// 一次调用的两份上下文：工具与判决链用路由后的；hooks 用会话的。
pub(crate) struct Routed {
    pub context: ToolUseContext,
    pub hooks: ToolUseContext,
    /// 远端调用的机器名（hooks 的 `machine`、询问的 `machine`）；本机为 None。
    pub machine: Option<String>,
}

impl Routed {
    /// 不换机器：两份都是原来的上下文。
    pub(crate) fn unchanged(context: ToolUseContext) -> Self {
        Self {
            hooks: context.clone(),
            context,
            machine: None,
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

    pub(crate) fn plan(&self, tool: &str, context: &ToolUseContext) -> Plan {
        let Some(registry) = &self.0 else {
            return Plan::Unchanged;
        };
        if !takes_machine(tool) {
            return if context.machine.is_local() {
                Plan::Unchanged
            } else {
                Plan::ToLocal
            };
        }
        let machines = registry.machines();
        if machines.is_empty() && context.machine.is_local() {
            return Plan::Unchanged;
        }
        Plan::Route { machines }
    }

    /// 输入点名的是清单里有、现在没连上的机器：给模型的“没有执行”说明（远端执行 F98，remote-exec.md §25.2.2）。
    ///
    /// 缺陷修复（Docker 试用 M2）：schema 校验在路由之前，没连上的机器不在工具定义里（没有远端机器时没有 `machine`，
    /// 有别的机器时 `enum` 里没有它），模型拿到的是 schema 错误，F96 的“没有执行”说明到不了；宽松对象的 Agent 甚至丢掉
    /// `machine`、在本机起子代理。现在先于 schema 校验认出它。不在清单里的名字照旧交给 schema 校验。
    pub(crate) fn not_connected(&self, tool: &str, input: &Value) -> Option<String> {
        let registry = self.0.as_ref()?;
        let name = named_in(input).filter(|_| takes_machine(tool))?;
        (registry.remote(name).is_none() && registry.listed(name))
            .then(|| texts::render(texts::remote_not_connected(), &[("machine", name)]))
    }

    /// 校验用的 schema：有远端机器连上时加上 `machine`（与发给模型的相同的取值）；否则是工具自己的。
    pub(crate) fn schema(base: Value, plan: &Plan) -> Value {
        match plan {
            Plan::Route { machines } if !machines.is_empty() => {
                // 描述不参与校验，这里用本机的写法。
                with_machine(&base, &choices(machines), texts::machine_param())
            }
            _ => base,
        }
    }

    /// 取出 `machine`、建两份上下文。机器没连上时是给模型的错误文字。
    pub(crate) fn route(
        &self,
        tool: &str,
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
                routed.machine = registry.local_machine();
                Ok(Routed {
                    context: routed,
                    hooks: context,
                    machine: None,
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
                    Some(Value::String(name)) => name.clone(),
                    Some(other) => {
                        return Err(format!("`{MACHINE}` must be a string, got {other}"));
                    }
                    None => context.machine.name().to_owned(),
                };
                let mut routed = context.clone();
                if target == LOCAL_MACHINE {
                    routed.machine = registry.local_machine();
                    return Ok(Routed {
                        context: routed,
                        hooks: context,
                        machine: None,
                    });
                }
                // 没连上的机器：写明没有执行、下一步做什么（远端执行 F96，remote-exec.md §23.6）。
                let Some((machine, workspace)) = registry.remote(&target) else {
                    return Err(texts::render(
                        texts::remote_not_connected(),
                        &[("machine", target.as_str())],
                    ));
                };
                routed.machine = machine;
                if tool != AGENT_TOOL {
                    routed.workspace = workspace;
                }
                Ok(Routed {
                    context: routed,
                    hooks: context,
                    machine: Some(target),
                })
            }
        }
    }
}

/// hooks 的 payload 加上 `machine`（只有远端调用，§20.8）。
pub(crate) fn tag(mut payload: Value, machine: Option<&str>) -> Value {
    if let (Some(name), Some(object)) = (machine, payload.as_object_mut()) {
        object.insert(MACHINE.to_owned(), Value::from(name));
    }
    payload
}
