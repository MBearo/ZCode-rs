//! 工具的 `machine` 参数（docs/specs/rust-cli/remote-exec.md §20.5、§20.6）：哪些工具带它、怎样加进发给模型的
//! schema、怎样从调用的输入里取出。描述文字在 `prompts/`，由调用方给。

use serde_json::{Map, Value, json};

use crate::env_registry::{LOCAL_MACHINE, MachineSet};

/// 参数名。
pub const MACHINE: &str = "machine";

/// Agent 工具：`machine` 是子代理的缺省机器（它自己在本机跑，工作区不换）。
pub const AGENT_TOOL: &str = "Agent";

/// 带 `machine` 参数的工具（暂定，remote-exec.md §20.5.1）：起后台任务的只有 Bash 与 Monitor，TaskStop 按任务所在的
/// 机器停，不带参数。
pub const MACHINE_TOOLS: &[&str] = &[
    "Bash",
    "Monitor",
    "Read",
    "Edit",
    "Write",
    "NotebookEdit",
    "Grep",
    "Glob",
    AGENT_TOOL,
];

/// 这个工具带不带 `machine` 参数。
pub fn takes_machine(tool: &str) -> bool {
    MACHINE_TOOLS.contains(&tool)
}

/// 参数的取值：`local` 在前，再是连上的机器名（按名字）。
pub fn choices(machines: &MachineSet) -> Vec<String> {
    let mut out = vec![LOCAL_MACHINE.to_owned()];
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

/// 工具输入里写的机器（只读，不去掉）：冷恢复按它把记录归到那台机器（remote-exec.md §20.7.3）。没写、或写的是本机时 None。
pub fn named_in(input: &Value) -> Option<&str> {
    input
        .get(MACHINE)
        .and_then(Value::as_str)
        .filter(|name| *name != LOCAL_MACHINE)
}

#[cfg(test)]
#[path = "machine_param_tests.rs"]
mod tests;
