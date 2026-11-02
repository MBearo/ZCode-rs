//! 工具的 `machine` 参数（docs/specs/rust-cli/remote-exec.md §20.5、§20.6）：工具怎样用它（[`MachineParam`]，工具自己经
//! [`crate::tool::Tool::machine_param`] 说）、怎样加进发给模型的 schema、怎样从调用的输入里取出。描述文字在 `prompts/`，
//! 由调用方给。

use serde_json::{Map, Value, json};

use crate::env_registry::MachineSet;
use crate::ids::{MachineName, PRIMARY_MACHINE};

/// 参数名。
pub const MACHINE: &str = "machine";

/// 工具怎样用 `machine` 参数（暂定，remote-exec.md §20.5.1）：带参数的是 Bash、Monitor、Read、Edit、Write、
/// NotebookEdit、Grep、Glob（在那台机器上执行）与 Agent（子代理在那台机器上执行）；起后台任务的只有 Bash 与 Monitor，
/// TaskStop 按任务所在的机器停，不带参数。
///
/// 工作线 F137：以前是按名字查的表 `MACHINE_TOOLS`（执行管线与主循环各查一次），Agent 的两处不同（不换工作区、参数
/// 说明的写法）又各按名字比较一次；加一个带参数的工具要改 kernel-api 的表。现在是工具自己的属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineParam {
    /// 不带参数（缺省）：调用在主 env 上（子代理在远端时，WebFetch、TaskStop、EnterWorktree 等也换回主 env）。
    Absent,
    /// 在点名的机器上执行：调用的上下文换成那台机器与它的工作区。
    RunsThere,
    /// 交给点名的机器（Agent）：工具自己在主 env 上跑，上下文只换机器、不换工作区；参数说明是另一种写法。
    Delegates,
}

/// 参数的取值：主 env（`primary`）在前，再是连上的机器名（按名字）。
pub fn choices(machines: &MachineSet) -> Vec<String> {
    let mut out = vec![PRIMARY_MACHINE.to_owned()];
    out.extend(machines.names());
    out
}

/// 在 `schema.properties` 的末尾加上 `machine`（`enum` 是 `choices`）。不是对象 schema 时原样返回。
pub fn with_machine(schema: &Value, choices: &[String], description: &str) -> Value {
    let mut schema = schema.clone();
    let Some(object) = schema.as_object_mut() else {
        return schema;
    };
    let properties = object
        .entry("properties")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(properties) = properties.as_object_mut() {
        properties.insert(
            MACHINE.to_owned(),
            json!({"type": "string", "enum": choices, "description": description}),
        );
    }
    schema
}

/// 从调用的输入里取出 `machine`（去掉这个键）。没有这个键、或输入不是对象时 None；值不是字符串时原样交回，由调用方报错。
pub fn take(input: &mut Value) -> Option<Value> {
    input.as_object_mut()?.remove(MACHINE)
}

/// 工具输入里写的机器（只读，不去掉）：冷恢复按它把记录归到那台机器（remote-exec.md §20.7.3）。没写、或写的是主 env
/// （含以前的名字 `local`）时 None。
pub fn named_in(input: &Value) -> Option<&str> {
    input
        .get(MACHINE)
        .and_then(Value::as_str)
        .filter(|name| !MachineName::is_primary_name(name))
}

#[cfg(test)]
#[path = "machine_param_tests.rs"]
mod tests;
