// 复制自 apps/zcode-cli-rust/crates/bash/src/lib.rs（07ab266d4b），只保留只读判定。
//! Bash 命令的只读判定（docs/specs/rust-cli/shell-permissions.md §4.3）。
//!
//! 表与回调来自 Node 版（`bash-readonly-policy-*.ts`：命令白名单、每个命令的安全参数、危险参数回调、
//! git 只读子命令）。Node 版的“总是允许”建议与规则求值没有复制：规则匹配在 [`crate::shell_rules`]。
mod argv;
mod callbacks;
mod callbacks_net;
mod flags;
mod git;
mod git_callbacks;
mod js;
mod printf;
mod tables;

pub(crate) use argv::strip_wrappers;
pub use git::invokes_git;

use crate::parse::{Analysis, analyze, is_permission_safe};

/// Node `isRuntimeReadOnlyBashCommand`：每个子命令都只读，且没有改目录的 git、没有动态内容。
/// `git_unsafe` 为 true 时（工作目录里的 git 配置不可信）含 git 的命令一律不算只读。
pub fn is_read_only_analysis(analysis: &Analysis, git_unsafe: bool) -> bool {
    if !is_permission_safe(analysis) || analysis.commands.is_empty() {
        return false;
    }
    if git::git_with_directory_change(&analysis.commands) {
        return false;
    }
    if git_unsafe && invokes_git(&analysis.commands) {
        return false;
    }
    analysis
        .commands
        .iter()
        .all(|part| !argv::has_known_write_option(part) && argv::evaluate(part) == Some(true))
}

/// [`is_read_only_analysis`] 的字符串版本。
pub fn is_read_only(command: &str, git_unsafe: bool) -> bool {
    is_read_only_analysis(&analyze(command), git_unsafe)
}

#[cfg(test)]
#[path = "readonly_tests.rs"]
mod tests;
