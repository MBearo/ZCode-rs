//! guarded 模式的危险命令询问（docs/specs/rust-cli/shell-permissions.md §4.6.1）：命中时判决链先询问一次（只批准这一次），
//! 没命中或认不出时同 bypassPermissions 放行。识别在 `shell::danger`（纯 shell 分析）。

use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::PermissionMode;

use crate::shell::danger::{Danger, Dialect, dangerous};

/// 这次调用要不要按 guarded 先询问一次：模式是 guarded、`command` 是判决主体为 shell 命令的工具（Bash、Monitor）的
/// 命令、命令命中规则（Node `!planEnabled && mode === "guarded" && userApprovalRule`，Node 只有 Bash；plan 开时判决
/// 用的模式是 plan，不会到这里）。
pub fn applies(mode: PermissionMode, command: Option<&str>, style: PathStyle) -> bool {
    if mode != PermissionMode::Guarded {
        return false;
    }
    command
        .is_some_and(|command| matches!(dangerous(command, Dialect::of(style)), Danger::Matched(_)))
}

#[cfg(test)]
#[path = "guarded_tests.rs"]
mod tests;
