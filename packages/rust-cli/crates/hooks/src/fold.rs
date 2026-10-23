//! 工具类事件的合并（docs/specs/rust-cli/hooks.md §4.3、§6）。
//!
//! 结果按完成顺序到达，逐个折叠（PreToolUse、PostToolUse、PostToolUseFailure、PermissionRequest 各一个
//! 函数）；聚合规则（权限优先级、updatedInput）在结果循环里逐个合并。

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::permission::PermissionDecision;
use serde_json::Value;

use crate::result::{Behavior, HookResult, Outcome, PermissionRequestDecision};
use crate::texts;

/// 折叠用到的公共信息。
pub(crate) struct Fold<'a> {
    pub ids: &'a dyn Ids,
    /// 事件名。
    pub event: &'a str,
    /// `<事件>[:<match query>]`。
    pub hook_name: &'a str,
}

impl Fold<'_> {
    pub(crate) fn context(
        &self,
        hook_name: &str,
        contexts: &[String],
    ) -> Option<rust_cli_kernel_api::message::Message> {
        texts::additional_context(self.ids, hook_name, self.event, contexts)
    }
}

/// 只给用户看的 `systemMessage`。
pub(crate) fn notices(results: &[HookResult]) -> Vec<String> {
    results
        .iter()
        .filter_map(|result| result.system_message.clone())
        .collect()
}

/// `ZA` 的权限聚合：deny > defer > ask > allow。
fn aggregate(current: Option<Behavior>, next: Behavior) -> Option<Behavior> {
    Some(match (current, next) {
        (_, Behavior::Deny) | (Some(Behavior::Deny), _) => Behavior::Deny,
        (_, Behavior::Defer) | (Some(Behavior::Defer), _) => Behavior::Defer,
        (_, Behavior::Ask) | (Some(Behavior::Ask), _) => Behavior::Ask,
        (_, Behavior::Allow) => Behavior::Allow,
    })
}

pub(crate) fn pre_tool_use(fold: &Fold<'_>, results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    let mut aggregated: Option<Behavior> = None;
    let mut decision: Option<PermissionDecision> = None;
    let mut decision_input: Option<Value> = None;
    let mut passthrough_input: Option<Value> = None;
    for result in results {
        if result.prevent {
            outcome.prevent_continuation = true;
            if result.stop_reason.is_some() {
                outcome.stop_reason.clone_from(&result.stop_reason);
            }
        }
        if let Some(blocking) = &result.blocking {
            let message = texts::pre_tool_blocking(fold.hook_name, blocking);
            outcome.blocking_error = Some(message.clone());
            decision = Some(PermissionDecision::Deny { message });
            decision_input = None;
            aggregated = Some(Behavior::Deny);
        }
        if let Some(behavior) = result.behavior {
            aggregated = aggregate(aggregated, behavior);
            // 只交出与当前聚合行为相同的决定。
            if aggregated == Some(behavior) {
                let reason = result.reason.clone();
                match behavior {
                    Behavior::Allow => {
                        decision = Some(PermissionDecision::Allow {
                            updated_input: None,
                        });
                        decision_input.clone_from(&result.updated_input);
                    }
                    Behavior::Ask => {
                        let message = reason.unwrap_or_else(|| {
                            texts::decision_default(fold.hook_name, "asked for confirmation for")
                        });
                        decision = Some(PermissionDecision::Ask {
                            message,
                            suggestions: Vec::new(),
                        });
                        decision_input.clone_from(&result.updated_input);
                    }
                    // 同一个 hook 已给出阻塞时保留阻塞文本：JSON deny 的结果是
                    // `PreToolUse:Bash hook error: <permissionDecisionReason>`，不是原因本身。
                    Behavior::Deny if result.blocking.is_some() => decision_input = None,
                    Behavior::Deny => {
                        let message = reason
                            .unwrap_or_else(|| texts::decision_default(fold.hook_name, "denied"));
                        decision = Some(PermissionDecision::Deny { message });
                        decision_input = None;
                    }
                    Behavior::Defer => {}
                }
            }
        } else if result.updated_input.is_some() {
            passthrough_input.clone_from(&result.updated_input);
        }
        if let Some(context) = &result.additional_context {
            outcome.additional_context.push(context.clone());
            outcome
                .messages
                .extend(fold.context(fold.hook_name, std::slice::from_ref(context)));
        }
    }
    outcome.decision = decision;
    // allow / ask 的 updatedInput 优先，否则用单独交出的。
    outcome.updated_input = decision_input.or(passthrough_input);
    if outcome.prevent_continuation {
        let message = outcome
            .stop_reason
            .clone()
            .unwrap_or_else(|| texts::STOPPED_BY_HOOK.to_owned());
        outcome.after_result.push(texts::stopped_continuation(
            fold.ids,
            fold.hook_name,
            fold.event,
            &message,
        ));
    }
    outcome
}

pub(crate) fn post_tool_use(fold: &Fold<'_>, results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    for result in results {
        if result.outcome == Outcome::Cancelled {
            continue;
        }
        if result.updated_tool_output.is_some() {
            outcome
                .updated_tool_output
                .clone_from(&result.updated_tool_output);
        }
        if result.prevent {
            let message = result
                .stop_reason
                .clone()
                .unwrap_or_else(|| texts::STOPPED_BY_POST_TOOL_USE.to_owned());
            outcome.prevent_continuation = true;
            outcome.stop_reason.clone_from(&result.stop_reason);
            outcome.messages.push(texts::stopped_continuation(
                fold.ids,
                fold.hook_name,
                fold.event,
                &message,
            ));
            // 遇到停止请求即返回，后面的结果不再处理。
            break;
        }
        if let Some(blocking) = &result.blocking {
            outcome
                .blocking_error
                .get_or_insert_with(|| blocking.error.clone());
            outcome.messages.push(texts::blocking_error(
                fold.ids,
                fold.hook_name,
                fold.event,
                blocking,
            ));
        }
        if let Some(context) = &result.additional_context {
            outcome.additional_context.push(context.clone());
            outcome
                .messages
                .extend(fold.context(fold.hook_name, std::slice::from_ref(context)));
        }
    }
    outcome
}

pub(crate) fn post_tool_use_failure(fold: &Fold<'_>, results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    for result in results {
        if let Some(blocking) = &result.blocking {
            outcome
                .blocking_error
                .get_or_insert_with(|| blocking.error.clone());
            outcome.messages.push(texts::blocking_error(
                fold.ids,
                fold.hook_name,
                fold.event,
                blocking,
            ));
        }
        if let Some(context) = &result.additional_context {
            outcome.additional_context.push(context.clone());
            outcome
                .messages
                .extend(fold.context(fold.hook_name, std::slice::from_ref(context)));
        }
    }
    outcome
}

/// 第一个给出决定的 hook 生效。
pub(crate) fn permission_request(results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    if let Some(decision) = results
        .iter()
        .find_map(|result| result.permission_request.clone())
    {
        match decision {
            PermissionRequestDecision::Allow { updated_input } => {
                outcome.decision = Some(PermissionDecision::Allow {
                    updated_input: None,
                });
                outcome.updated_input = updated_input;
            }
            PermissionRequestDecision::Deny { message } => {
                outcome.decision = Some(PermissionDecision::Deny {
                    message: message
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| texts::PERMISSION_DENIED_BY_HOOK.to_owned()),
                });
            }
        }
    }
    outcome
}

/// PermissionDenied：任何 hook 给出 `retry: true` 即重试。
pub(crate) fn permission_denied(results: &[HookResult]) -> HookOutcome {
    HookOutcome {
        notices: notices(results),
        retry: results.iter().any(|result| result.retry),
        ..HookOutcome::default()
    }
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
