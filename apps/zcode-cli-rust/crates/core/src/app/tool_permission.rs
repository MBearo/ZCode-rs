//! Permission gate of one tool call (Node `permission-flow.ts` `resolveToolPermission`).
use crate::contract::{Event, EventSink, PermissionAnswer, ToolOutput, ToolPort};
use anyhow::{Result, bail};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) type Permissions =
    tokio::sync::watch::Receiver<std::sync::Arc<super::permissions::Snapshot>>;

/// Node `resolveToolPermission`: `None` to run the tool, otherwise the tool
/// result the model reads instead (a refusal, or a failure after allow).
pub(super) async fn authorize(
    tools: &dyn ToolPort,
    permissions: &Permissions,
    call: &Value,
    args: &Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<Option<ToolOutput>> {
    use crate::domain::permission::Behavior;
    let name = call["function"]["name"].as_str().unwrap_or("");
    let snapshot = permissions.borrow().clone();
    let permission = tools.permission(&sink.session_id, name, args).await;
    let capability = &permission.capability;
    let rules = permission
        .rules
        .as_deref()
        .map(|r| r as &dyn crate::domain::permission::RulePolicy);
    let decision = snapshot.check(name, args, capability, rules);
    match decision.behavior {
        Behavior::Allow => Ok(None),
        Behavior::Deny => Ok(Some(refusal(super::permissions::summarize(
            &decision.reason,
        )))),
        // AskUserQuestion 的询问就是工具自身的问答交互（Node userInput 通道），不再单独弹权限。
        Behavior::Ask if name == "AskUserQuestion" => Ok(None),
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
                    reason: decision.reason,
                    input: args.clone(),
                    suggestions: permission.suggestions.clone(),
                    options_policy,
                },
                reply,
            })
            .await?;
            let answer = tokio::select! {biased;
                _=cancel.cancelled()=>bail!("Cancelled"),
                result=receipt=>result.map_err(|_| anyhow::anyhow!("Cancelled"))?,
            };
            Ok(match answer {
                PermissionAnswer::Allow => None,
                PermissionAnswer::Deny { message, preserve } => Some(refusal(if preserve {
                    message
                } else {
                    super::permissions::summarize(&message)
                })),
                PermissionAnswer::Fail(message) => {
                    let mut output = ToolOutput::text(message);
                    output.failed = true;
                    Some(output)
                }
            })
        }
    }
}

/// A denied call: the model reads the reason, the row ends `cancelled`.
fn refusal(message: String) -> ToolOutput {
    let mut output = ToolOutput::text(message);
    output.failed = true;
    output.denied = true;
    output
}
