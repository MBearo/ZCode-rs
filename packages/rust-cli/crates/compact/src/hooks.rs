//! 压缩的 PreCompact / PostCompact hooks（docs/specs/rust-cli/hooks.md §7、compact.md §7）。

use rust_cli_kernel_api::message::{CompactTrigger, Message};
use rust_cli_kernel_api::orchestration::{
    CompactRequest, HookContext, HookEvent, HookEventName, HookRunner,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::engine::Failure;

fn trigger_name(trigger: CompactTrigger) -> &'static str {
    // 反应式压缩以 `auto` 调 hook。
    match trigger {
        CompactTrigger::Manual => "manual",
        CompactTrigger::Auto | CompactTrigger::Reactive => "auto",
    }
}

/// hook 给的说明接在用户说明后面。
fn merge_instructions(user: Option<&str>, hook: Option<String>) -> Option<String> {
    match (
        user.filter(|text| !text.is_empty()),
        hook.filter(|text| !text.is_empty()),
    ) {
        (Some(user), Some(hook)) => Some(format!("{user}\n\n{hook}")),
        (Some(user), None) => Some(user.to_owned()),
        (None, hook) => hook,
    }
}

/// 压缩 hook 的上下文：模式与子代理（hooks.md §7）。
fn hook_context(request: &CompactRequest) -> HookContext {
    HookContext {
        mode: request.mode,
        agent: request.agent.clone(),
        // prompt hook 的缺省模型经小模型槽位从它得出（model.md §6.1）。
        selection: Some(request.selection.clone()),
        ..HookContext::default()
    }
}

/// PreCompact hook（hooks.md §7）。
pub(crate) async fn pre_compact(
    hooks: &dyn HookRunner,
    request: &CompactRequest,
    cancel: &CancellationToken,
) -> Result<Option<String>, Failure> {
    let event = HookEvent {
        name: HookEventName::PreCompact,
        session: request.session.clone(),
        workspace: request.workspace.clone(),
        payload: json!({
            "trigger": trigger_name(request.trigger),
            "custom_instructions": request.instructions,
        }),
        context: hook_context(request),
    };
    let outcome = hooks.run(event, cancel.clone()).await;
    if let Some(reason) = outcome.blocking_error {
        return Err(Failure::Blocked(reason));
    }
    // hooks crate 已把成功 hook 的输出合并成 `custom_instructions`（hooks.md §7）。
    Ok(merge_instructions(
        request.instructions.as_deref(),
        outcome.custom_instructions,
    ))
}

/// SessionStart（`compact`）：压缩成功后、PostCompact 之前（spec §7）。
/// 载荷只有 `source` 与 `model`（对 compact 不传标题）；返回要排在附件之后提交的 hook 消息。
pub(crate) async fn session_start(
    hooks: &dyn HookRunner,
    request: &CompactRequest,
    cancel: &CancellationToken,
) -> Vec<Message> {
    let event = HookEvent {
        name: HookEventName::SessionStart,
        session: request.session.clone(),
        workspace: request.workspace.clone(),
        payload: json!({"source": "compact", "model": request.selection.model}),
        context: HookContext {
            agent: request.agent.clone(),
            ..HookContext::default()
        },
    };
    hooks.run(event, cancel.clone()).await.messages
}

/// PostCompact hook：结果只用于显示（spec §7）。
pub(crate) async fn post_compact(
    hooks: &dyn HookRunner,
    request: &CompactRequest,
    summary: &str,
    cancel: &CancellationToken,
) {
    let event = HookEvent {
        name: HookEventName::PostCompact,
        session: request.session.clone(),
        workspace: request.workspace.clone(),
        payload: json!({
            "trigger": trigger_name(request.trigger),
            "compact_summary": summary,
        }),
        context: hook_context(request),
    };
    let _ = hooks.run(event, cancel.clone()).await;
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
