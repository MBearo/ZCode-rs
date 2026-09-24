//! Permission gate of one tool call (Node `permission-flow.ts` `resolveToolPermission`).
use super::hook_runner::Hooks;
use super::tool_hooks::{Prompt, ToolCall};
use crate::contract::{Event, EventSink, PermissionAnswer, ToolOutput, ToolPort};
use crate::domain::hooks::{HookEvent, decision, output::RunResult};
use anyhow::{Result, bail};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) type Permissions =
    tokio::sync::watch::Receiver<std::sync::Arc<super::permissions::Snapshot>>;

/// The gate's verdict.
pub(super) enum Gate {
    /// Run the tool; `Some` replaces its input (PermissionRequest `modify`).
    Run(Option<Value>),
    /// The result the model reads instead (a refusal, or a failure after allow).
    Stop(ToolOutput),
}

/// Node `resolveToolPermission`. `hooks` carries the run's hooks and the
/// merged PreToolUse result, which may turn an `ask` into `allow` or the
/// reverse before the policy result is used.
pub(super) async fn authorize(
    tools: &dyn ToolPort,
    permissions: &Permissions,
    call: &Value,
    args: &Value,
    hooks: Option<(&Arc<Hooks>, &RunResult)>,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<Gate> {
    use crate::domain::permission::Behavior;
    let name = call["function"]["name"].as_str().unwrap_or("");
    let id = call["id"].as_str().unwrap_or("");
    let snapshot = permissions.borrow().clone();
    let permission = tools.permission(&sink.session_id, name, args).await;
    let capability = &permission.capability;
    let rules = permission
        .rules
        .as_deref()
        .map(|r| r as &dyn crate::domain::permission::RulePolicy);
    let mut decision = snapshot.check(name, args, capability, rules);
    if let Some((_, pre)) = hooks {
        decision::apply_pre_tool(&mut decision, pre);
    }
    // Node withPlanExitDeniedTurnStop：plan 开启时 ExitPlanMode 被拒绝（非反馈）即停轮。
    let plan_exit = name == crate::domain::plan_mode::EXIT && snapshot.state.plan_enabled;
    let stop = |mut output: ToolOutput| {
        output.stop_turn |= plan_exit;
        Gate::Stop(output)
    };
    match decision.behavior {
        Behavior::Allow => Ok(Gate::Run(None)),
        Behavior::Deny => Ok(stop(refusal(super::permissions::summarize(
            &decision.reason,
        )))),
        // AskUserQuestion 的询问就是工具自身的问答交互（Node userInput 通道），不再单独弹权限；
        // 无头运行没有问答通道，与 Node 一样经权限询问被拒绝。
        Behavior::Ask if name == "AskUserQuestion" && !snapshot.headless => Ok(Gate::Run(None)),
        Behavior::Ask => {
            let ask_options = capability
                .permission
                .as_ref()
                .and_then(|p| p.ask_options.as_ref());
            let options_policy = match ask_options.map(|o| &o["allowAlways"]) {
                Some(Value::Bool(false)) => Some("no-always-allow".to_owned()),
                Some(Value::String(scope)) if scope == "session" => {
                    Some("session-always-allow".to_owned())
                }
                _ => None,
            };
            let (reply, receipt) = oneshot::channel();
            sink.send(Event::Permission {
                call: call.clone(),
                request: crate::contract::PermissionRequest {
                    reason: decision.reason.clone(),
                    risk_level: decision.risk_level.clone(),
                    input: args.clone(),
                    suggestions: permission.suggestions.clone(),
                    options_policy,
                },
                reply,
            })
            .await?;
            let racing = hooks
                .map(|(hooks, _)| hooks)
                .filter(|hooks| hooks.handles(HookEvent::PermissionRequest));
            let (answer, modified) = match racing {
                Some(hooks) => {
                    let call = ToolCall {
                        id,
                        name,
                        args,
                        mode: snapshot.state.mode.as_str(),
                    };
                    let prompt = Prompt {
                        snapshot: &snapshot,
                        asked: &decision,
                        receipt,
                    };
                    let raced =
                        super::tool_hooks::race(hooks, tools, &call, prompt, sink, cancel).await?;
                    (raced.answer, raced.modified)
                }
                None => {
                    let answer = tokio::select! {biased;
                        _=cancel.cancelled()=>bail!("Cancelled"),
                        result=receipt=>result.map_err(|_| anyhow::anyhow!("Cancelled"))?,
                    };
                    (answer, None)
                }
            };
            Ok(match answer {
                PermissionAnswer::Allow => Gate::Run(modified),
                PermissionAnswer::Deny { message, preserve } => stop(refusal(if preserve {
                    message
                } else {
                    super::permissions::summarize(&message)
                })),
                // 与 Node turn-control 一致：带反馈的拒绝由 Engine 引导进同一轮；无反馈则停轮。
                PermissionAnswer::PlanRejected(feedback) => {
                    let text = if feedback.is_some() {
                        crate::domain::plan_mode::NOT_APPROVED
                    } else {
                        crate::domain::plan_mode::DENIED
                    };
                    let mut output = refusal(text.into());
                    output.stop_turn = feedback.is_none();
                    Gate::Stop(output)
                }
                PermissionAnswer::Fail(message) => {
                    let mut output = ToolOutput::text(message);
                    output.failed = true;
                    Gate::Stop(output)
                }
            })
        }
    }
}

/// A denied call: the model reads the reason, the row ends `cancelled`.
pub(super) fn refusal(message: String) -> ToolOutput {
    let mut output = ToolOutput::text(message);
    output.failed = true;
    output.denied = true;
    output
}
