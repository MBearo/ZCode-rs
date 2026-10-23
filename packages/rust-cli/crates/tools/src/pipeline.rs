//! 工具执行管线（校验、hooks、权限、调用与结果预算）。
//!
//! 顺序：schema 校验 → 工具自己的 `validate_input` → PreToolUse hooks → 权限判决（需要时先跑
//! PermissionRequest hooks，再询问用户）→ 执行 → PostToolUse / PostToolUseFailure hooks。任何一步拒绝
//! 都返回 `is_error` 的结果，不进入下一步。hook 的决定与判决链的先后、hook 消息的位置见
//! docs/specs/rust-cli/hooks.md §6。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::js::string::utf16_prefix;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{
    HookEventName, HookRunner, InteractionRequest, PermissionAnswer, PermissionGate, ToolExecution,
    ToolExecutor,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionRule};
use rust_cli_kernel_api::tool::{
    Tool, ToolCallOutput, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use rust_cli_kernel_api::unparsed_input;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::budget;
use crate::pipeline_hooks::{self as hooks, HookMessages};
use crate::texts;
use crate::validate::{strip_unknown, validate};

pub(crate) const REJECT_MESSAGE: &str = rust_cli_kernel_api::tool::REJECT_MESSAGE;
pub(crate) const REJECT_WITH_REASON: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\n";
/// 工具使用被中断时的文字（kernel-api 的定义，F55）。
const INTERRUPT_FOR_TOOL_USE: &str = rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[1];
const NO_OUTPUT: &str = "Command failed with no output";
/// `formatError` 超过这个长度（UTF-16 单元）时保留首尾各一半。
const ERROR_LIMIT: usize = 10_000;

/// 执行管线；权限判决链与 hooks 由 runtime 注入。
pub struct Pipeline {
    gate: Arc<dyn PermissionGate>,
    hooks: Arc<dyn HookRunner>,
}

/// 子代理里的询问被拒时的结果文字（主线程是 [`REJECT_MESSAGE`]，docs/specs/rust-cli/agents.md §4.1.1）。
const SUBAGENT_REJECT: &str = "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). Try a different approach or report the limitation to complete your task.";
const SUBAGENT_REJECT_WITH_REASON: &str = "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user said:\n";

/// 用户拒绝（或撤回询问）时给模型的文字：子代理里的调用用子代理的那一段。
///
/// 修复线 F67：以前子代理与主线程同一段（`The user doesn't want to proceed …`）；子代理被拒时应该换个做法或报告限制，
/// 而不是停下来等用户。
fn rejection(context: &ToolUseContext, feedback: Option<&str>) -> String {
    match (context.agent.is_some(), feedback) {
        (false, None) => REJECT_MESSAGE.to_owned(),
        (false, Some(feedback)) => format!("{REJECT_WITH_REASON}{feedback}"),
        (true, None) => SUBAGENT_REJECT.to_owned(),
        (true, Some(feedback)) => format!("{SUBAGENT_REJECT_WITH_REASON}{feedback}"),
    }
}

/// 拒绝的来源：PreToolUse hook 拒绝时，它的停止说明随结果交出（只在这类拒绝后交出）。
enum Rejection {
    ByHook(ToolOutput),
    Other(ToolOutput),
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: true,
        display: Value::Null,
    }
}

/// 用户拒绝时的结果（拒绝文本、`is_error`）；提问与计划审批的工具在 `call` 里用它。
pub(crate) fn rejected(text: String) -> ToolOutput {
    error(text)
}

fn wrapped(text: &str) -> ToolOutput {
    error(format!("<tool_use_error>{text}</tool_use_error>"))
}

/// 错误文本：空消息换成固定文本，过长时保留首尾各 5000 个 UTF-16 单元。
fn format_error(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        return NO_OUTPUT.to_owned();
    }
    let length = message.encode_utf16().count();
    if length <= ERROR_LIMIT {
        return message.to_owned();
    }
    let half = ERROR_LIMIT / 2;
    let start = utf16_prefix(message, half);
    let end_units = length - half;
    let end = &message[utf16_prefix(message, end_units).len()..];
    format!(
        "{start}\n\n... [{} characters truncated] ...\n\n{end}",
        length - ERROR_LIMIT
    )
}

impl Pipeline {
    pub fn new(gate: Arc<dyn PermissionGate>, hooks: Arc<dyn HookRunner>) -> Self {
        Self { gate, hooks }
    }

    /// 判决链或 hook 改过的输入要重新通过 schema。
    fn revalidate(tool: &dyn Tool, schema: &Value, input: Value) -> Result<Value, ToolOutput> {
        match validate(tool.name(), schema, &input) {
            Some(message) => Err(wrapped(&format!("InputValidationError: {message}"))),
            None => Ok(input),
        }
    }

    /// 询问用户；先跑 PermissionRequest hooks（D10：跑完再决定是否询问）。返回 hook 改过的输入。
    /// 取消时按拒绝处理（中断的结果由执行器统一替换）。
    async fn ask(
        &self,
        tool: &dyn Tool,
        input: &Value,
        message: String,
        suggestions: &[PermissionRule],
        context: &ToolUseContext,
    ) -> Result<Option<Value>, ToolOutput> {
        // 修复线 F20：guarded 的危险命令只批准这一次（Node `approvalMode: "user-once"` 时 `runHooks: false`）：
        // 不跑 PermissionRequest hooks，hook 不能替用户批准；没有“总是允许”（判决链不给建议规则）。
        if !self.gate.ask_once(tool, input, context).await {
            let payload = hooks::permission_request(tool.name(), input, suggestions);
            let event = hooks::event(HookEventName::PermissionRequest, context, payload).await;
            let request = self.hooks.run(event, context.cancel.clone()).await;
            match request.decision {
                Some(PermissionDecision::Allow { .. }) => return Ok(request.updated_input),
                Some(PermissionDecision::Deny { message }) => return Err(error(message)),
                Some(PermissionDecision::Ask { .. } | PermissionDecision::Passthrough) | None => {}
            }
        }
        let request = InteractionRequest::Permission {
            tool_use: context.tool_use.clone(),
            tool: tool.name().to_owned(),
            input: input.clone(),
            message: message.clone(),
            suggestions: suggestions.to_vec(),
            asked_by: None,
        };
        let Ok(answer) = crate::asking::ask::<PermissionAnswer>(context, request).await else {
            return Err(error(rejection(context, None)));
        };
        match answer {
            Some(PermissionAnswer::Allow { remember }) => {
                // “总是允许”：会话 actor 已把规则与交互的解决同一批落库；判决链按 `/permissions` 的路径读回库值、换上
                // LocalSettings 层（shell-permissions.md §5）。修复（R1-2）：以前这里交给判决链另存一份按会话的规则，
                // 子代理与 `/clear` 之后的新会话看不到，又问一次。
                if !remember.is_empty() {
                    self.gate.local_rules_committed(&context.workspace).await;
                }
                Ok(None)
            }
            Some(PermissionAnswer::Deny {
                feedback: Some(feedback),
            }) if !feedback.trim().is_empty() => Err(error(rejection(context, Some(&feedback)))),
            // 没有人能回答（-p）：模型收到询问本身的文本。
            Some(PermissionAnswer::Unavailable) => Err(error(message)),
            // 修 bug（工作线 F69）：`-p` 的后台子代理以前也给询问本身的文本；它所在的上下文不能询问，PermissionRequest
            // hooks（上面已跑）没有结论时给“不能询问”的拒绝文字（agents.md §4.1.1）。
            Some(PermissionAnswer::NoPrompts) => Err(error(
                rust_cli_policy::texts::prompts_unavailable(tool.name()),
            )),
            Some(PermissionAnswer::Deny { .. }) | None => Err(error(rejection(context, None))),
        }
    }

    /// 询问并换上 PermissionRequest hook 改过的输入。
    async fn ask_for(
        &self,
        tool: &dyn Tool,
        schema: &Value,
        input: Value,
        message: String,
        suggestions: &[PermissionRule],
        context: &ToolUseContext,
    ) -> Result<Value, ToolOutput> {
        match self
            .ask(tool, &input, message, suggestions, context)
            .await?
        {
            Some(updated) => Self::revalidate(tool, schema, updated),
            None => Ok(input),
        }
    }

    /// 完整的权限判决链。
    async fn decide(
        &self,
        tool: &dyn Tool,
        schema: &Value,
        input: Value,
        context: &ToolUseContext,
    ) -> Result<Value, ToolOutput> {
        match self.gate.decide(tool, &input, context).await {
            PermissionDecision::Allow {
                updated_input: Some(updated),
            } => Self::revalidate(tool, schema, updated),
            PermissionDecision::Allow {
                updated_input: None,
            } => Ok(input),
            PermissionDecision::Deny { message } => Err(error(message)),
            PermissionDecision::Ask {
                message,
                suggestions,
            } => {
                self.ask_for(tool, schema, input, message, &suggestions, context)
                    .await
            }
            PermissionDecision::Passthrough => {
                let message = format!(
                    "ZCode requested permissions to use {}, but you haven't granted it yet.",
                    tool.name()
                );
                self.ask_for(tool, schema, input, message, &[], context)
                    .await
            }
        }
    }

    /// 校验、PreToolUse hooks 与权限；通过时返回最终输入。
    async fn admit(
        &self,
        tool: &dyn Tool,
        mut input: Value,
        context: &ToolUseContext,
        messages: &mut HookMessages,
    ) -> Result<Value, Rejection> {
        // R5-1：输入在组装时不是合法 JSON（标记对象，kernel-api `unparsed_input`）：先于 schema 校验
        // 给出 JSON 解析失败的说明，不跑 hooks 与权限。
        if let Some(unparsed) = unparsed_input::parse(&input) {
            let message = texts::unparsed_input(tool.name(), unparsed.raw, unparsed.len);
            return Err(Rejection::Other(wrapped(&format!(
                "InputValidationError: {message}"
            ))));
        }
        let schema = tool.input_schema();
        // 修复线 F51（agents.md §2）：宽松对象的工具（Agent）丢掉 schema 外的键、不报错，之后的 hook、
        // `validate_input` 与 `call` 都用丢掉之后的输入。以前一律按严格对象，
        // 模型给 Agent 传了 schema 外的键（例如 F51 删掉的 `model`）就报 InputValidationError。
        if tool.strips_unknown_keys() {
            input = strip_unknown(&schema, input);
        }
        if let Some(message) = validate(tool.name(), &schema, &input) {
            return Err(Rejection::Other(wrapped(&format!(
                "InputValidationError: {message}"
            ))));
        }
        if let Validation::Invalid { message, .. } = tool.validate_input(&input, context).await {
            return Err(Rejection::Other(wrapped(&message)));
        }
        let payload = hooks::pre_tool_use(tool.name(), &input, context);
        let event = hooks::event(HookEventName::PreToolUse, context, payload).await;
        let mut pre = self.hooks.run(event, context.cancel.clone()).await;
        hooks::take_pre(messages, &mut pre);
        if let Some(updated) = pre.updated_input.take() {
            if let Some(message) = validate(tool.name(), &schema, &updated) {
                return Err(Rejection::ByHook(error(format!(
                    "PreToolUse hook for {} returned updatedInput that failed schema validation: {message}",
                    tool.name()
                ))));
            }
            input = updated;
        }
        let other = Rejection::Other;
        match pre.decision {
            Some(PermissionDecision::Deny { message }) => Err(Rejection::ByHook(error(message))),
            // hook 放行：deny / ask 规则与安全路径仍然生效（不可绕过的权限关卡）。
            Some(PermissionDecision::Allow { .. }) => {
                match self.gate.check_rules(tool, &input, context).await {
                    Some(PermissionDecision::Deny { message }) => Err(other(error(message))),
                    Some(PermissionDecision::Ask { .. }) => self
                        .decide(tool, &schema, input, context)
                        .await
                        .map_err(other),
                    Some(PermissionDecision::Allow { .. } | PermissionDecision::Passthrough)
                    | None => Ok(input),
                }
            }
            // hook 要求询问：用 hook 的原因询问。
            Some(PermissionDecision::Ask {
                message,
                suggestions,
            }) => match self.gate.check_rules(tool, &input, context).await {
                Some(PermissionDecision::Deny { message }) => Err(other(error(message))),
                Some(PermissionDecision::Ask { .. }) => self
                    .decide(tool, &schema, input, context)
                    .await
                    .map_err(other),
                Some(PermissionDecision::Allow { .. } | PermissionDecision::Passthrough) | None => {
                    self.ask_for(tool, &schema, input, message, &suggestions, context)
                        .await
                        .map_err(other)
                }
            },
            Some(PermissionDecision::Passthrough) | None => self
                .decide(tool, &schema, input, context)
                .await
                .map_err(other),
        }
    }

    /// 执行并跑 PostToolUse / PostToolUseFailure hooks。
    async fn call(
        &self,
        tool: &dyn Tool,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
        messages: &mut HookMessages,
    ) -> ToolOutput {
        match tool
            .call_with_messages(input.clone(), context.clone(), progress)
            .await
        {
            Ok(ToolCallOutput {
                mut output,
                mut new_messages,
            }) => {
                let payload = hooks::post_tool_use(tool.name(), &input, &output, &context);
                let event = hooks::event(HookEventName::PostToolUse, &context, payload).await;
                let mut post = self.hooks.run(event, context.cancel.clone()).await;
                messages.after.append(&mut post.messages);
                // 工具自己的后续消息（Skill 的正文）排在 PostToolUse 的消息之后
                // （docs/specs/rust-cli/ext.md §2.4）。
                messages.after.append(&mut new_messages);
                if let Some(replacement) = post.updated_tool_output.take() {
                    hooks::replace_output(&mut output, replacement);
                }
                // hook 看到的是原始结果；给模型的结果再按预算落盘（在 hook 之后持久化）。
                budget::apply(
                    output,
                    tool.name(),
                    tool.max_result_size_chars(),
                    &context.session,
                    context.artifacts.as_ref(),
                )
                .await
            }
            Err(failure) => {
                let interrupted = matches!(failure, ToolError::Cancelled);
                let text = if interrupted {
                    INTERRUPT_FOR_TOOL_USE.to_owned()
                } else {
                    format_error(&failure.to_string())
                };
                let payload =
                    hooks::post_tool_use_failure(tool.name(), &input, &text, interrupted, &context);
                let event =
                    hooks::event(HookEventName::PostToolUseFailure, &context, payload).await;
                let mut post = self.hooks.run(event, context.cancel.clone()).await;
                messages.after.append(&mut post.messages);
                error(text)
            }
        }
    }
}

#[async_trait]
impl ToolExecutor for Pipeline {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolOutput {
        self.execute_full(tool, input, context, progress)
            .await
            .output
    }

    async fn execute_full(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        let mut messages = HookMessages::default();
        let input = match self
            .admit(tool.as_ref(), input, &context, &mut messages)
            .await
        {
            Ok(input) => input,
            Err(Rejection::ByHook(output)) => return messages.finish(output, true),
            Err(Rejection::Other(output)) => return messages.finish(output, false),
        };
        let output = self
            .call(tool.as_ref(), input, context, progress, &mut messages)
            .await;
        messages.finish(output, true)
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pipeline_hook_tests.rs"]
mod hook_tests;

#[cfg(test)]
#[path = "pipeline_message_tests.rs"]
mod message_tests;
