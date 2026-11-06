//! 权限询问的 Always allow（docs/specs/rust-cli/shell-permissions.md §4.10、tui.md §5.2，工作线 F72）：Bash 有种子时
//! ⇥ 把它放进输入框改（`interaction_keys.rs`），记住的规则由改过的前缀得出；没改时记住全部建议规则。
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionRule;

use crate::view::BASH;

/// 询问框弹出时的种子：内核在询问里给出（Bash 且有建议规则时才有，F133：以前这里经 runtime 调判决链的命令分析）。
pub(in crate::ui) fn seed(request: &InteractionRequest) -> Option<String> {
    match request {
        InteractionRequest::Permission { rule_seed, .. } => rule_seed.clone(),
        InteractionRequest::Question { .. } | InteractionRequest::PlanApproval { .. } => None,
    }
}

/// 种子（可能被用户改过）记住的规则：`*` 是整个 Bash，空的只允许这一次。
pub(in crate::ui) fn rules(seed: &str) -> Vec<PermissionRule> {
    match seed.trim() {
        "" => Vec::new(),
        "*" => vec![PermissionRule {
            tool: BASH.to_owned(),
            content: None,
        }],
        prefix => vec![PermissionRule {
            tool: BASH.to_owned(),
            content: Some(prefix.to_owned()),
        }],
    }
}

#[cfg(test)]
#[path = "interaction_remember_tests.rs"]
mod tests;
