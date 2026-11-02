//! “总是允许”的落库（docs/specs/rust-cli/shell-permissions.md §5）：用户在权限询问里选“不再询问”时，
//! 规则与交互的解决在同一批提交（存储写进本机项目规则集），提交成功后才唤醒等待的工具。

use rust_cli_kernel_api::orchestration::{Answer, InteractionRequest};
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::store::Fact;

/// 权限询问的“允许并记住”回答要加的事实；其余回答没有。
pub(crate) fn remembered(request: &InteractionRequest, answer: &Answer) -> Option<Fact> {
    let InteractionRequest::Permission { .. } = request else {
        return None;
    };
    let rules = answer.remembered();
    (!rules.is_empty()).then(|| Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: rules.to_vec(),
    })
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
