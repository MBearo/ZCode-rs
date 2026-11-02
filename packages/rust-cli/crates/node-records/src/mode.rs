//! 权限模式与 Node 执行状态的映射（docs/specs/rust-cli/README.md §5，node-store.md §4.3）。
//!
//! Node 的执行状态是 `{mode, planEnabled}`，`mode` 只能是 build / edit / yolo / guarded / auto，
//! plan 是独立的开关；会话行的 `permission.mode` 还接受 `plan`。修复线 F20 起内核也是两个值
//! （`ExecutionState`），这里一一对应；guarded 读写都是 guarded（以前读成 bypassPermissions）。
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use serde_json::Value;

/// Node `executionPermissionModeSchema`。
const NODE_MODES: [&str; 5] = ["build", "edit", "yolo", "guarded", "auto"];

/// Node `ExecutionState`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    pub mode: String,
    pub plan: bool,
}

impl Default for Execution {
    fn default() -> Self {
        Self {
            mode: "build".into(),
            plan: false,
        }
    }
}

/// 会话行 `permission.mode` 的取值（spec §5 的映射；dontAsk 记为 build，另有 `rust-cli/` entry）。
pub fn permission_value(mode: PermissionMode) -> &'static str {
    mode.node_name()
}

/// 内核的执行状态 → Node `ExecutionState`（修复线 F20）。
pub fn to_node(state: ExecutionState) -> Execution {
    Execution {
        mode: permission_value(state.mode).to_owned(),
        plan: state.plan,
    }
}

/// Node `resolveExecutionState(input, current)`：合法模式替换当前模式并关掉 plan；`plan` 只打开
/// plan，保留当前模式；其余值不改变当前状态。
pub fn resolve(input: Option<&str>, current: &Execution) -> Execution {
    match input {
        Some(mode) if NODE_MODES.contains(&mode) => Execution {
            mode: mode.to_owned(),
            plan: false,
        },
        Some("plan") => Execution {
            mode: current.mode.clone(),
            plan: true,
        },
        _ => current.clone(),
    }
}

/// Node `executionStateSchema.safeParse`（非 strict：多余字段忽略）。
pub fn parse(data: &Value) -> Option<Execution> {
    let mode = data["mode"].as_str().filter(|m| NODE_MODES.contains(m))?;
    let plan = data["planEnabled"].as_bool()?;
    Some(Execution {
        mode: mode.to_owned(),
        plan,
    })
}

/// Node 执行状态对应的内核执行状态（修复线 F20：plan 是开关，guarded 是 guarded；以前 plan 读成一个模式、
/// guarded 读成 bypassPermissions，进入 plan 前的模式丢掉）。
pub fn from_node(state: &Execution) -> ExecutionState {
    // `plan` 是开关不是模式，与不认的值一样读成 default（名字表在 kernel-api，F58）。
    let mode = PermissionMode::from_node_name(&state.mode)
        .filter(|mode| *mode != PermissionMode::Plan)
        .unwrap_or(PermissionMode::Default);
    ExecutionState {
        mode,
        plan: state.plan,
    }
}

/// 权限模式 `mode` 写入后，执行状态的 `mode` 就是 `state.mode`：用来判断 `rust-cli/permission_mode` entry 是否仍然
/// 有效（Node 续写时改了模式，entry 就过期了）。plan 开关另记，不参与比较；旧行里记的 `plan` 不算权限模式。
pub fn matches(mode: PermissionMode, state: &Execution) -> bool {
    mode != PermissionMode::Plan && state.mode == permission_value(mode)
}

/// 读回的执行状态：`rust-cli/permission_mode` 与执行状态一致时用它定权限模式（区分 default 与 dontAsk）。
pub fn read(state: &Execution, named: Option<PermissionMode>) -> ExecutionState {
    let base = from_node(state);
    match named.filter(|named| matches(*named, state)) {
        Some(mode) => ExecutionState { mode, ..base },
        None => base,
    }
}

/// 内核的模式名（`rust-cli/permission_mode` entry 的 `data.mode`）。
pub fn entry_name(mode: PermissionMode) -> &'static str {
    mode.cli_name()
}

pub fn from_entry_name(name: &str) -> Option<PermissionMode> {
    serde_json::from_value(Value::String(name.to_owned())).ok()
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;
