//! 会话类事件与“外部”路径的合并（docs/specs/rust-cli/hooks.md §4.3、§7）。
//!
//! Stop / SubagentStop、UserPromptSubmit、SessionStart、PreCompact / PostCompact、SessionEnd 各有自己的
//! 合并方式（见各函数）。

use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use serde_json::Value;

use crate::fold::{Fold, notices};
use crate::result::{HookResult, Summary};
use crate::texts;

pub(crate) fn stop(fold: &Fold<'_>, results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    for result in results {
        if result.prevent {
            let message = result
                .stop_reason
                .clone()
                .unwrap_or_else(|| texts::STOP_PREVENTED.to_owned());
            outcome.prevent_continuation = true;
            outcome.stop_reason = Some(message.clone());
            // 停止说明固定用 hookName `Stop`。
            outcome.messages.push(texts::stopped_continuation(
                fold.ids, "Stop", "Stop", &message,
            ));
        }
        if let Some(blocking) = &result.blocking {
            outcome
                .blocking_error
                .get_or_insert_with(|| blocking.error.clone());
            outcome
                .messages
                .push(texts::stop_feedback(fold.ids, blocking));
        }
        if let Some(context) = &result.additional_context {
            outcome.additional_context.push(context.clone());
            outcome
                .messages
                .extend(fold.context(fold.event, std::slice::from_ref(context)));
        }
    }
    outcome
}

/// `prompt` 是这次提交的输入文本（阻塞说明里要带上）。
pub(crate) fn user_prompt_submit(
    fold: &Fold<'_>,
    results: &[HookResult],
    prompt: &str,
) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    for result in results {
        if let Some(blocking) = &result.blocking {
            let text = texts::prompt_blocked(blocking, prompt, result.suppress_original_prompt);
            outcome.blocking_error = Some(blocking.error.clone());
            outcome.notices.insert(0, text);
            outcome.messages.clear();
            return outcome;
        }
        if result.prevent {
            let text = match &result.stop_reason {
                Some(reason) => format!("{}: {reason}", texts::OPERATION_STOPPED),
                None => texts::OPERATION_STOPPED.to_owned(),
            };
            outcome.prevent_continuation = true;
            outcome.stop_reason.clone_from(&result.stop_reason);
            outcome
                .messages
                .push(texts::meta(fold.ids, "hook_stopped", text.clone()));
            outcome.notices.insert(0, text);
            return outcome;
        }
        if result.session_title.is_some() {
            outcome.session_title.clone_from(&result.session_title);
        }
        if let Some(content) = result.success_content.as_deref().filter(|c| !c.is_empty()) {
            outcome.messages.push(texts::success(
                fold.ids,
                fold.hook_name,
                fold.event,
                content,
            ));
        }
        if let Some(context) = &result.additional_context {
            outcome.additional_context.push(context.clone());
            outcome
                .messages
                .extend(fold.context(fold.event, std::slice::from_ref(context)));
        }
    }
    outcome
}

/// SessionStart：附加上下文合并成最后一条（hookName `SessionStart`）；SubagentStart 同样合并。
pub(crate) fn session_start(fold: &Fold<'_>, results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    for result in results {
        if let Some(content) = result.success_content.as_deref().filter(|c| !c.is_empty())
            && fold.event == HookEventName::SessionStart.as_str()
        {
            outcome.messages.push(texts::success(
                fold.ids,
                fold.hook_name,
                fold.event,
                content,
            ));
        }
        if let Some(context) = &result.additional_context {
            outcome.additional_context.push(context.clone());
        }
        if result.initial_user_message.is_some() {
            outcome
                .initial_user_message
                .clone_from(&result.initial_user_message);
        }
        if result.session_title.is_some() {
            outcome.session_title.clone_from(&result.session_title);
        }
        outcome
            .watch_paths
            .extend(result.watch_paths.iter().cloned());
    }
    let contexts = outcome.additional_context.clone();
    outcome.messages.extend(fold.context(fold.event, &contexts));
    outcome
}

/// 其他 REPL 事件：每个 hook 的附加上下文一条，停止与第一个阻塞原因交给调用方。
pub(crate) fn generic(fold: &Fold<'_>, results: &[HookResult]) -> HookOutcome {
    let mut outcome = HookOutcome {
        notices: notices(results),
        ..HookOutcome::default()
    };
    for result in results {
        if result.prevent {
            outcome.prevent_continuation = true;
            if result.stop_reason.is_some() {
                outcome.stop_reason.clone_from(&result.stop_reason);
            }
        }
        if let Some(blocking) = &result.blocking {
            outcome
                .blocking_error
                .get_or_insert_with(|| blocking.error.clone());
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

/// PreCompact / PostCompact 的用户提示行。
fn compact_lines(event: &str, summaries: &[Summary], skip_blocked: bool) -> Vec<String> {
    summaries
        .iter()
        .filter(|summary| !summary.cancelled)
        .map(|summary| {
            let output = summary.output.trim();
            let ok = summary.succeeded && !(skip_blocked && summary.blocked);
            match (ok, output.is_empty()) {
                (true, false) => format!(
                    "{event} [{}] completed successfully: {output}",
                    summary.command
                ),
                (true, true) => format!("{event} [{}] completed successfully", summary.command),
                (false, false) => format!("{event} [{}] failed: {output}", summary.command),
                (false, true) => format!("{event} [{}] failed", summary.command),
            }
        })
        .collect()
}

/// “外部”路径（结果按配置顺序）。
pub(crate) fn outside(event: HookEventName, summaries: &[Summary]) -> HookOutcome {
    let mut outcome = HookOutcome::default();
    match event {
        HookEventName::PreCompact => {
            let instructions: Vec<&str> = summaries
                .iter()
                .filter(|s| s.succeeded && !s.blocked && !s.output.trim().is_empty())
                .map(|s| s.output.trim())
                .collect();
            if !instructions.is_empty() {
                outcome.custom_instructions = Some(instructions.join("\n\n"));
            }
            let lines = compact_lines("PreCompact", summaries, true);
            if !lines.is_empty() {
                outcome.notices.push(lines.join("\n"));
            }
            let blocked: Vec<String> = summaries
                .iter()
                .filter(|s| s.blocked)
                .map(|s| match s.output.trim() {
                    "" => format!("[{}]", s.command),
                    output => format!("[{}]: {output}", s.command),
                })
                .collect();
            if !blocked.is_empty() {
                outcome.blocking_error = Some(blocked.join("\n"));
            }
        }
        HookEventName::PostCompact => {
            let lines = compact_lines("PostCompact", summaries, false);
            if !lines.is_empty() {
                outcome.notices.push(lines.join("\n"));
            }
        }
        HookEventName::SessionEnd => {
            outcome.notices = summaries
                .iter()
                .filter(|s| !s.succeeded && !s.output.is_empty())
                .map(|s| format!("SessionEnd hook [{}] failed: {}", s.command, s.output))
                .collect();
        }
        _ => {
            if let Some(blocked) = summaries.iter().find(|s| s.blocked) {
                outcome.blocking_error = Some(blocked.output.clone());
            }
        }
    }
    outcome
}

/// UserPromptSubmit 的提交文本。
pub(crate) fn prompt_of(payload: &Value) -> &str {
    payload
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "fold_session_tests.rs"]
mod tests;
