//! prompt hook：经模型端口评估条件（docs/specs/rust-cli/hooks.md §5.3、goal.md §5）。
//!
//! prompt 里的 `$ARGUMENTS` 换成输入 JSON，
//! system 是身份句加固定的评估说明，`output_config.format` 是 `{ok, reason, impossible}` 的 json_schema。Stop /
//! SubagentStop 先套 “Based on the conversation transcript above …” 并带上对话记录（`/goal`）。

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::hooks::StopVerdict;
use rust_cli_kernel_api::ids::{AgentId, RunId, SessionId};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, CacheControl, ModelEvent, ModelPort, ModelRequest,
    ModelSelection, OutputFormat, QuerySource, RequestOptions, SystemBlock,
};
use rust_cli_prompts::texts::{identity, render};
use rust_cli_prompts::{goal, models};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::PromptHook;
use crate::result::{Blocking, HookResult, Outcome};
use crate::texts;

/// prompt hook 的缺省超时（30 秒）：配置里的 prompt hook 与 `/goal` 的停止条件共用（F55：以前写了两次）。
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// 没有模型卡片时的输出上限：同主循环的缺省（prompt hook 不传上限，按模型的缺省）。修复线 F58：
/// 以前注释写“与旁路请求相同”（旁路是 8192），值一直是 32000；现在取 prompts 的一个常量。
const DEFAULT_MAX_TOKENS: u32 = rust_cli_prompts::models::UNKNOWN_MAX_OUTPUT_TOKENS;
/// 对话记录里已有同一条件的反馈时，条件截到的长度。
const REPEATED_PROMPT_CHARS: usize = 500;

/// `$ARGUMENTS` 的替换：`$ARGUMENTS` 换成输入 JSON，`\$ARGUMENTS` 保留字面量；
/// 没有占位符时末尾附上 `ARGUMENTS: <输入>`。
pub(crate) fn with_arguments(prompt: &str, input: &str) -> String {
    const TOKEN: &str = "$ARGUMENTS";
    let mut out = String::new();
    let mut rest = prompt;
    let mut replaced = false;
    while let Some(found) = rest.find(TOKEN) {
        let (before, after) = rest.split_at(found);
        if let Some(head) = before.strip_suffix('\\') {
            out.push_str(head);
            out.push_str(TOKEN);
        } else {
            out.push_str(before);
            out.push_str(input);
            replaced = true;
        }
        rest = &after[TOKEN.len()..];
    }
    out.push_str(rest);
    if !replaced {
        out.push_str("\n\nARGUMENTS: ");
        out.push_str(input);
    }
    out
}

/// 去掉首尾的代码围栏。
fn strip_fence(text: &str) -> &str {
    let text = text.trim();
    let text = match text.strip_prefix("```") {
        Some(rest) => rest
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim_start(),
        None => text,
    };
    text.strip_suffix("```").map_or(text, str::trim_end).trim()
}

/// 超过 `limit` 个 UTF-16 码元时截断（不拆代理对），标出去掉的长度。
fn truncate(text: &str, limit: usize) -> String {
    let total: usize = text.chars().map(char::len_utf16).sum();
    if total <= limit {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars() {
        if used + c.len_utf16() > limit {
            break;
        }
        used += c.len_utf16();
        kept.push(c);
    }
    format!("{kept}\u{2026} [+{} chars]", total - used)
}

fn ephemeral() -> Option<CacheControl> {
    Some(CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: None,
        scope: None,
    })
}

/// 发请求的一方：hook 所在的会话（工作线 F52：`metadata.user_id` 的 `session_id`，model.md §3.3）、触发它的回合与
/// 子代理（工作线 F64：每请求头的 `x-query-id`、`x-zcode-session-type`，model.md §2.4）。
#[derive(Clone, Copy)]
pub(crate) struct Requester<'a> {
    pub session: &'a SessionId,
    pub run: Option<&'a RunId>,
    pub agent: Option<&'a AgentId>,
}

/// 评估请求（旁路请求的形状）：身份句与 system 各带断点；对话记录的断点去掉，只有最后一个块带。`configured_output`
/// 是目录的输出上限（模型端口的能力）：借卡片形状的 GLM 按它（工作线 F121，model.md §11.1）。
fn request(
    (selection, requester): (ModelSelection, Requester<'_>),
    system: &str,
    mut messages: Vec<ApiMessage>,
    (user, configured_output): (String, Option<u64>),
) -> ModelRequest {
    let max_tokens = models::card_output_limit(&selection.model, configured_output)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    for message in &mut messages {
        for content in &mut message.content {
            content.cache_control = None;
        }
    }
    let question = ApiContent {
        block: ContentBlock::Text { text: user },
        cache_control: ephemeral(),
    };
    match messages.last_mut() {
        Some(last) if last.role == ApiRole::User => last.content.push(question),
        _ => messages.push(ApiMessage {
            role: ApiRole::User,
            content: vec![question],
            output_config: None,
        }),
    }
    let block = |text: &str| SystemBlock {
        text: text.to_owned(),
        cache_control: ephemeral(),
    };
    ModelRequest {
        system: vec![block(identity()), block(system)],
        messages,
        tools: Vec::new(),
        tool_choice: None,
        max_tokens,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source: QuerySource::Side("hook_prompt".to_owned()),
        options: RequestOptions {
            // 修 bug（F23）：没有卡片的模型以前也写选择的档位（`card_effort` 的注释）。
            effort: models::card_effort(&selection.model, selection.reasoning.as_deref()),
            output_format: serde_json::from_value::<OutputFormat>(goal::stop_format()).ok(),
            session_id: Some(requester.session.clone()),
            run: requester.run.cloned(),
            agent: requester.agent.cloned(),
            ..RequestOptions::default()
        },
        selection,
    }
}

/// 评估模型的回答。
enum Answer {
    Met(Option<String>),
    NotMet(Option<String>),
    Impossible(Option<String>),
}

/// 一次评估的结果。
enum Asked {
    Answer(Answer),
    Failed(String),
    TimedOut,
    Cancelled,
}

async fn complete(
    model: &dyn ModelPort,
    request: ModelRequest,
    cancel: CancellationToken,
) -> Result<String, String> {
    let mut stream = model.stream(request, cancel);
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event.map_err(|error| error.to_string())? {
            ModelEvent::BlockStart {
                block: ContentBlock::Text { text: start },
                ..
            } => text.push_str(&start),
            ModelEvent::BlockDelta {
                delta: BlockDelta::Text(delta),
                ..
            } => text.push_str(&delta),
            _ => {}
        }
    }
    Ok(text)
}

/// 发请求、按评估的 schema 解析（`ok` 必填，`reason` / `impossible` 可缺）。`stop` 时 `impossible` 有效。
async fn ask(
    model: &dyn ModelPort,
    request: ModelRequest,
    timeout_ms: u64,
    stop: bool,
    cancel: CancellationToken,
) -> Asked {
    let child = cancel.child_token();
    let response = tokio::select! {
        response = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            complete(model, request, child.clone()),
        ) => response,
        () = cancel.cancelled() => return Asked::Cancelled,
    };
    let text = match response {
        Err(_) => {
            child.cancel();
            return Asked::TimedOut;
        }
        Ok(Err(message)) => return Asked::Failed(format!("Hook evaluator API error: {message}")),
        Ok(Ok(text)) => text,
    };
    let Ok(Value::Object(json)) = serde_json::from_str::<Value>(strip_fence(text.trim())) else {
        return Asked::Failed("JSON validation failed".to_owned());
    };
    let Some(ok) = json.get("ok").and_then(Value::as_bool) else {
        return Asked::Failed("Schema validation failed: ok must be a boolean".to_owned());
    };
    let reason = match json.get("reason") {
        None => None,
        Some(Value::String(reason)) => Some(reason.clone()),
        Some(_) => {
            return Asked::Failed("Schema validation failed: reason must be a string".to_owned());
        }
    };
    let impossible = match json.get("impossible") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Asked::Failed(
                "Schema validation failed: impossible must be a boolean".to_owned(),
            );
        }
    };
    Asked::Answer(match (ok, impossible && stop) {
        (true, _) => Answer::Met(reason),
        (false, true) => Answer::Impossible(reason),
        (false, false) => Answer::NotMet(reason),
    })
}

/// 选模型：hook 写了 `model` 时只换模型名，没写时用缺省模型。
fn selection(hook_model: Option<&String>, default: &ModelSelection) -> ModelSelection {
    ModelSelection {
        model: hook_model.cloned().unwrap_or_else(|| default.model.clone()),
        ..default.clone()
    }
}

/// 阻塞的文字：`[<prompt>]: <reason>`（JS 模板里缺省的 reason 是 `undefined`）。
fn blocking(prompt: &str, reason: Option<&str>) -> Blocking {
    Blocking {
        error: format!("[{prompt}]: {}", reason.unwrap_or("undefined")),
        command: prompt.to_owned(),
    }
}

pub(crate) struct PromptRun<'a> {
    pub model: Option<&'a dyn ModelPort>,
    /// hook 所在的会话（`HookContext.session`）、触发它的回合与子代理（`HookContext.run` / `agent`）。
    pub requester: Requester<'a>,
    pub default_model: Option<&'a ModelSelection>,
    pub hook: &'a PromptHook,
    pub input: &'a str,
    pub timeout_ms: u64,
    /// 配置里的 Stop / SubagentStop prompt hook：没有对话记录，不支持（D8）。
    pub stop_event: bool,
}

fn error(message: impl std::fmt::Display) -> HookResult {
    HookResult::non_blocking(format!("Error executing prompt hook: {message}"))
}

/// 配置的 prompt hook（非 Stop 事件）。
pub(crate) async fn run(run: PromptRun<'_>, cancel: CancellationToken) -> HookResult {
    if run.stop_event {
        return error("prompt hooks for Stop and SubagentStop are not supported yet");
    }
    let (Some(model), Some(default)) = (run.model, run.default_model) else {
        return error("no model is configured for prompt hooks");
    };
    let selection = selection(run.hook.model.as_ref(), default);
    let configured = model
        .capabilities(&selection)
        .await
        .and_then(|found| found.max_output_tokens);
    let request = request(
        (selection, run.requester),
        goal::prompt_system(),
        Vec::new(),
        (with_arguments(&run.hook.prompt, run.input), configured),
    );
    match ask(model, request, run.timeout_ms, false, cancel).await {
        Asked::Cancelled | Asked::TimedOut => HookResult::cancelled(),
        Asked::Failed(message) => HookResult::non_blocking(message),
        Asked::Answer(Answer::Met(reason) | Answer::Impossible(reason)) => HookResult {
            stop_reason: reason,
            success_content: Some(String::new()),
            ..HookResult::default()
        },
        Asked::Answer(Answer::NotMet(reason)) => {
            let blocked = blocking(&run.hook.prompt, reason.as_deref());
            HookResult {
                outcome: Outcome::Blocking,
                prevent: !run.hook.continue_on_block,
                stop_reason: reason,
                ..HookResult::blocking(blocked.error, blocked.command)
            }
        }
    }
}

/// `/goal` 的停止条件（会话级 Stop prompt hook，goal.md §5）。
pub(crate) struct StopRun<'a> {
    pub model: Option<&'a dyn ModelPort>,
    /// `/goal` 所在的会话、触发它的回合与子代理。
    pub requester: Requester<'a>,
    pub default_model: Option<&'a ModelSelection>,
    pub ids: &'a dyn Ids,
    pub condition: &'a str,
    pub input: &'a str,
    pub transcript: Vec<ApiMessage>,
    pub timeout_ms: u64,
}

/// 对话记录里有没有含 `needle` 的 user 消息。
fn mentioned(transcript: &[ApiMessage], needle: &str) -> bool {
    transcript
        .iter()
        .filter(|message| message.role == ApiRole::User)
        .flat_map(|message| &message.content)
        .any(|content| matches!(&content.block, ContentBlock::Text { text } if text.contains(needle)))
}

pub(crate) async fn evaluate(run: StopRun<'_>, cancel: CancellationToken) -> StopVerdict {
    let (Some(model), Some(default)) = (run.model, run.default_model) else {
        return StopVerdict::Failed {
            message: "Error executing prompt hook: no model is configured for prompt hooks"
                .to_owned(),
        };
    };
    // 反馈里条件的写法要在发请求前判断：之后对话记录归请求所有。
    let shown = if mentioned(&run.transcript, &format!("[{}]:", run.condition)) {
        truncate(run.condition, REPEATED_PROMPT_CHARS)
    } else {
        run.condition.to_owned()
    };
    let user = with_arguments(
        &render(goal::stop_condition(), &[("condition", run.condition)]),
        run.input,
    );
    let configured = model
        .capabilities(default)
        .await
        .and_then(|found| found.max_output_tokens);
    let request = request(
        (default.clone(), run.requester),
        goal::stop_system(),
        run.transcript,
        (user, configured),
    );
    match ask(model, request, run.timeout_ms, true, cancel).await {
        Asked::Cancelled => StopVerdict::Cancelled,
        Asked::TimedOut => StopVerdict::TimedOut,
        Asked::Failed(message) => StopVerdict::Failed { message },
        Asked::Answer(Answer::Met(reason)) => StopVerdict::Met { reason },
        Asked::Answer(Answer::Impossible(reason)) => StopVerdict::Impossible { reason },
        Asked::Answer(Answer::NotMet(reason)) => StopVerdict::NotMet {
            feedback: texts::stop_feedback(run.ids, &blocking(&shown, reason.as_deref())),
            reason,
        },
    }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
