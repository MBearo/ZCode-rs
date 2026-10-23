//! “总是允许”的落库（docs/specs/rust-cli/shell-permissions.md §5）：用户在权限询问里选“不再询问”时，
//! 规则与交互的解决在同一批提交（存储写进本机项目规则集），提交成功后才唤醒等待的工具。

use rust_cli_kernel_api::orchestration::{InteractionRequest, PermissionAnswer};
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::store::Fact;
use serde_json::Value;

/// 权限询问的“允许并记住”回答要加的事实；其余回答没有。
pub(crate) fn remembered(request: &InteractionRequest, answer: &Value) -> Option<Fact> {
    let InteractionRequest::Permission { .. } = request else {
        return None;
    };
    match serde_json::from_value::<PermissionAnswer>(answer.clone()).ok()? {
        PermissionAnswer::Allow { remember } if !remember.is_empty() => {
            Some(Fact::PermissionRulesAdded {
                behavior: RuleBehavior::Allow,
                rules: remember,
            })
        }
        PermissionAnswer::Allow { .. }
        | PermissionAnswer::Deny { .. }
        | PermissionAnswer::Unavailable
        | PermissionAnswer::NoPrompts => None,
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
