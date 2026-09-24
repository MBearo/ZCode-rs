//! Hooks around one tool call (Node `hook-flow.ts` and its `call-runner.ts` /
//! `permission-flow.ts` call sites): PreToolUse before the permission check,
//! PermissionRequest racing the user, PostToolUse / PostToolUseFailure after.
use super::hook_runner::Hooks;
use crate::contract::{Event, EventSink, PermissionAnswer, ToolOutput, ToolPort};
use crate::domain::hooks::{
    decision::{self, RequestAnswer},
    display, input,
    output::RunResult,
    tool_match_values,
};
use crate::domain::permission::{Behavior, Decision, Update};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// The call a tool event is about, with the input hooks and policy see.
pub(super) struct ToolCall<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub args: &'a Value,
    /// Collaboration mode at the call (Node `mode`).
    pub mode: &'a str,
}

impl ToolCall<'_> {
    fn call(&self) -> input::Call<'_> {
        input::Call {
            id: self.id,
            name: self.name,
            input: self.args,
        }
    }
}

pub(super) async fn pre_tool_use(
    hooks: &Hooks,
    tools: &dyn ToolPort,
    call: &ToolCall<'_>,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> RunResult {
    let capability = tools.capability(&sink.session_id, call.name, call.args);
    let now = hooks.timestamp();
    let risk = capability.risk_level.as_deref().unwrap_or("medium");
    let scope = capability.side_effect_scope.as_deref();
    let hook_input = input::pre_tool_use(
        &hooks.base(sink, call.mode, &now),
        &call.call(),
        risk,
        scope,
    );
    hooks
        .run(hook_input, &tool_match_values(call.name), sink, cancel)
        .await
}

pub(super) async fn post_tool_use(
    hooks: &Hooks,
    call: &ToolCall<'_>,
    output: &ToolOutput,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> RunResult {
    let now = hooks.timestamp();
    // Node 传入 handler 的原始输出；Rust 工具的结构化结果在 data，没有时用文本结果。
    let response = if output.data.is_null() {
        Value::from(output.content.clone())
    } else {
        output.data.clone()
    };
    let hook_input = input::post_tool_use(
        &hooks.base(sink, call.mode, &now),
        &call.call(),
        &response,
        None,
    );
    hooks
        .run(hook_input, &tool_match_values(call.name), sink, cancel)
        .await
}

pub(super) async fn post_tool_use_failure(
    hooks: &Hooks,
    call: &ToolCall<'_>,
    message: &str,
    cancelled: bool,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> RunResult {
    let now = hooks.timestamp();
    let kind = if cancelled {
        "tool_cancelled"
    } else {
        "tool_execution_failed"
    };
    let hook_input = input::post_tool_use_failure(
        &hooks.base(sink, call.mode, &now),
        &call.call(),
        message,
        kind,
    );
    hooks
        .run(hook_input, &tool_match_values(call.name), sink, cancel)
        .await
}

/// Node `appendPreToolAdditionalContextsToErrorResult` / `appendHookAdditionalContexts`.
pub(super) fn append_contexts(output: &mut ToolOutput, contexts: &[String]) {
    if !contexts.is_empty() {
        let text = display::tool_contexts(contexts);
        output.content = format!("{}\n\n{}", output.content, text);
        // Node 结构化内容：hook 上下文作为结尾的文本块。
        if let Some(serde_json::Value::Array(blocks)) = &mut output.model_content {
            blocks.push(serde_json::json!({"type": "text", "text": text}));
        }
    }
}

/// Node: PreToolUse `deny` / blocked continuation refuses the call before the
/// permission check; the model reads the hook's reason.
pub(super) fn pre_tool_refusal(pre: &RunResult) -> Option<String> {
    (pre.behavior == Some(Behavior::Deny) || pre.prevent_continuation).then(|| {
        pre.decision_reason
            .clone()
            .or_else(|| pre.stop_reason.clone())
            .unwrap_or_else(|| "Blocked by PreToolUse hook".into())
    })
}

/// A registered permission prompt the hooks race against.
pub(super) struct Prompt<'a> {
    pub snapshot: &'a super::permissions::Snapshot,
    /// The policy's `ask` decision.
    pub asked: &'a Decision,
    pub receipt: oneshot::Receiver<PermissionAnswer>,
}

/// What PermissionRequest hooks racing the prompt produced.
pub(super) struct Raced {
    pub answer: PermissionAnswer,
    /// `modify`: the call runs with this input when allowed.
    pub modified: Option<Value>,
}

/// Node `racePermissionResponders`: the prompt is already registered; hooks
/// answer it only if they finish first, and the losing hook chain is cancelled
/// without being awaited. A hook chain without a decision forfeits.
pub(super) async fn race(
    hooks: &Arc<Hooks>,
    tools: &dyn ToolPort,
    call: &ToolCall<'_>,
    prompt: Prompt<'_>,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<Raced> {
    let Prompt {
        snapshot,
        asked,
        mut receipt,
    } = prompt;
    let now = hooks.timestamp();
    let request_id = format!("perm_{}", hooks.clock.id());
    let reason = if asked.reason.is_empty() {
        format!("Tool {} requires approval", call.name)
    } else {
        asked.reason.clone()
    };
    let scope = (!asked.side_effect_scope.is_empty()).then_some(asked.side_effect_scope.as_str());
    let hook_input = input::permission_request(
        &hooks.base(sink, call.mode, &now),
        &call.call(),
        &reason,
        &request_id,
        &asked.risk_level,
        scope,
    );
    let chain_cancel = cancel.child_token();
    let mut chain = tokio::spawn({
        let hooks = hooks.clone();
        let sink = sink.clone();
        let token = chain_cancel.clone();
        let values = tool_match_values(call.name);
        async move { hooks.run(hook_input, &values, &sink, &token).await }
    });
    let mut chain_done = false;
    let mut modified = None;
    loop {
        tokio::select! {biased;
            _=cancel.cancelled()=>{
                chain_cancel.cancel();
                bail!("Cancelled");
            }
            answer=&mut receipt=>{
                // 用户先答复：取消 hook 链但不等待它（其生命周期事件仍会上报）。
                chain_cancel.cancel();
                let answer = answer.map_err(|_| anyhow!("Cancelled"))?;
                let modified = matches!(answer, PermissionAnswer::Allow).then_some(modified).flatten();
                return Ok(Raced { answer, modified });
            }
            result=&mut chain, if !chain_done=>{
                chain_done = true;
                let Some(answer) = result.ok().as_ref().and_then(decision::request_answer) else {
                    continue;
                };
                let (answer, updates, input) = settle(tools, snapshot, call, answer, sink).await;
                let Some(answer) = answer else {
                    // 改写后的输入命中项目 ask 规则：交互保持待决，由用户决定是否按改写后的输入执行。
                    modified = input;
                    continue;
                };
                let (accepted, taken) = oneshot::channel();
                sink.send(Event::PermissionHook {
                    call_id: call.id.into(),
                    answer,
                    updates,
                    accepted,
                })
                .await?;
                if taken.await.unwrap_or(false) {
                    modified = input;
                }
            }
        }
    }
}

/// A hook decision as the prompt's answer. `modify` is rechecked against the
/// policy first (Node `recheckPermissionHookModifiedInput`); `None` keeps the
/// prompt pending for the user.
async fn settle(
    tools: &dyn ToolPort,
    snapshot: &super::permissions::Snapshot,
    call: &ToolCall<'_>,
    answer: RequestAnswer,
    sink: &EventSink,
) -> (Option<PermissionAnswer>, Vec<Update>, Option<Value>) {
    let updates = |value: Option<Value>| -> Vec<Update> {
        value
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    };
    let deny = |message: String| PermissionAnswer::Deny {
        message,
        preserve: false,
    };
    match answer {
        RequestAnswer::Deny(reason) => (Some(deny(reason)), vec![], None),
        RequestAnswer::Allow(list) => (Some(PermissionAnswer::Allow), updates(list), None),
        RequestAnswer::Modify {
            input,
            updates: list,
        } => {
            let permission = tools.permission(&sink.session_id, call.name, &input).await;
            let rules = permission
                .rules
                .as_deref()
                .map(|r| r as &dyn crate::domain::permission::RulePolicy);
            let checked = snapshot.check(call.name, &input, &permission.capability, rules);
            match checked.behavior {
                Behavior::Deny => (Some(deny(checked.reason)), vec![], None),
                Behavior::Ask if checked.rule_id == "rule.project.ask" => {
                    (None, vec![], Some(input))
                }
                _ => (Some(PermissionAnswer::Allow), updates(list), Some(input)),
            }
        }
    }
}
