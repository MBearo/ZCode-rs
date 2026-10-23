//! 执行管线里的 hooks（docs/specs/rust-cli/hooks.md §6）。
//!
//! 事件的 payload 固定；PreToolUse 的决定与判决链的先后见 hooks.md §6。hook 的消息与停止请求随
//! [`ToolExecution`] 交给主循环。

use rust_cli_kernel_api::hooks::{HookContext, HookEvent, HookEventName, HookOutcome};
use rust_cli_kernel_api::message::{Message, ToolResultContent};
use rust_cli_kernel_api::orchestration::ToolExecution;
use rust_cli_kernel_api::permission::PermissionRule;
use rust_cli_kernel_api::tool::{ToolOutput, ToolUseContext};
use serde_json::{Value, json};

/// 一次调用里 hook 产生的消息（排在 tool_result 前后）。
#[derive(Default)]
pub(crate) struct HookMessages {
    pub before: Vec<Message>,
    pub after: Vec<Message>,
    /// PreToolUse 的停止说明；只有调用执行了、或被 hook 拒绝时才交出。
    pub pre_stop: Vec<Message>,
}

impl HookMessages {
    pub(crate) fn finish(self, output: ToolOutput, keep_pre_stop: bool) -> ToolExecution {
        let mut after = self.after;
        if keep_pre_stop {
            after.extend(self.pre_stop);
        }
        // 停止请求只有一个来源：消息里有 `hook_stopped_continuation`。
        let prevent_continuation = after.iter().any(|message| {
            matches!(message, Message::Attachment(a) if a.source == "hook_stopped_continuation")
        });
        ToolExecution {
            output,
            before: self.before,
            after,
            prevent_continuation,
        }
    }
}

/// 工具事件：带上会话当前的主模型（向会话 actor 取，prompt hook 的缺省模型经小模型槽位，model.md §6.1）。
pub(crate) async fn event(
    name: HookEventName,
    context: &ToolUseContext,
    payload: Value,
) -> HookEvent {
    let selection = context
        .interactions
        .current_selection(&context.session)
        .await;
    HookEvent {
        name,
        session: context.session.clone(),
        workspace: context.workspace.clone(),
        payload,
        context: HookContext {
            mode: Some(context.mode),
            agent: context.agent.clone(),
            agent_type: None,
            cwd: None,
            run: Some(context.run.clone()),
            selection,
        },
    }
}

pub(crate) fn pre_tool_use(tool: &str, input: &Value, context: &ToolUseContext) -> Value {
    json!({"tool_name": tool, "tool_input": input, "tool_use_id": context.tool_use})
}

pub(crate) fn post_tool_use(
    tool: &str,
    input: &Value,
    output: &ToolOutput,
    context: &ToolUseContext,
) -> Value {
    // tool_response 是给模型的内容块（D3）。
    json!({
        "tool_name": tool,
        "tool_input": input,
        "tool_response": output.content,
        "tool_use_id": context.tool_use,
    })
}

pub(crate) fn post_tool_use_failure(
    tool: &str,
    input: &Value,
    error: &str,
    interrupted: bool,
    context: &ToolUseContext,
) -> Value {
    json!({
        "tool_name": tool,
        "tool_input": input,
        "tool_use_id": context.tool_use,
        "error": error,
        "is_interrupt": interrupted,
    })
}

/// PermissionRequest 的 payload；建议规则换成 `PermissionUpdate` 的形状。
pub(crate) fn permission_request(
    tool: &str,
    input: &Value,
    suggestions: &[PermissionRule],
) -> Value {
    let mut payload = json!({"tool_name": tool, "tool_input": input});
    if !suggestions.is_empty() {
        let rules: Vec<Value> = suggestions
            .iter()
            .map(|rule| match &rule.content {
                Some(content) => json!({"toolName": rule.tool, "ruleContent": content}),
                None => json!({"toolName": rule.tool}),
            })
            .collect();
        payload["permission_suggestions"] = json!([{
            "type": "addRules",
            "rules": rules,
            "behavior": "allow",
            "destination": "localSettings",
        }]);
    }
    payload
}

/// PostToolUse 的 `updatedToolOutput`：字符串换成文本，内容块数组原样，其他值转成 JSON 文本（D3）。
pub(crate) fn replace_output(output: &mut ToolOutput, value: Value) {
    output.content = match value {
        Value::String(text) => vec![ToolResultContent::Text { text }],
        Value::Array(_) => serde_json::from_value(value.clone()).unwrap_or_else(|_| {
            vec![ToolResultContent::Text {
                text: value.to_string(),
            }]
        }),
        other => vec![ToolResultContent::Text {
            text: other.to_string(),
        }],
    };
}

/// PreToolUse 的消息分到前后两处。
pub(crate) fn take_pre(messages: &mut HookMessages, outcome: &mut HookOutcome) {
    messages.before.append(&mut outcome.messages);
    messages.pre_stop.append(&mut outcome.after_result);
}
