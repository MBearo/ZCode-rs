//! stream-json 事件的形状（docs/specs/rust-cli/headless.md §3.3–§3.6）：键序固定，
//! 只有纯函数。`serde_json` 打开了 `preserve_order`，`json!` 的键按写出的顺序输出。

use std::collections::BTreeMap;

use rust_cli_kernel_api::catalog::SessionCatalog;
use rust_cli_kernel_api::manage::{McpServerInfo, McpServerState};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, ImageSource, StopReason, ToolResultContent, Usage, UserMessage,
    UserOrigin,
};
use serde_json::{Map, Value, json};

/// 一个事件的 uuid 与时间（`toISOString`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) uuid: String,
    pub(crate) timestamp: String,
}

/// system/init 的内容（headless.md §3.3）。
pub(crate) struct Init<'a> {
    pub(crate) cwd: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) catalog: &'a SessionCatalog,
    pub(crate) mcp_servers: &'a [McpServerInfo],
    pub(crate) model: &'a str,
    /// 权限模式的名字（`default`、`acceptEdits`、`plan` 等）。
    pub(crate) permission_mode: &'a str,
    pub(crate) api_key_source: &'a str,
    pub(crate) version: &'a str,
}

/// `-p` 认的本地命令（`slash_commands` 里列出，headless.md §3.3）。
pub(crate) const LOCAL_COMMANDS: [&str; 3] = ["context", "usage", "rename"];

/// Agent 工具在清单里写成旧名 `Task`。
fn tool_name(name: &str) -> &str {
    if name == "Agent" { "Task" } else { name }
}

fn mcp_status(state: McpServerState) -> &'static str {
    match state {
        McpServerState::Pending => "pending",
        McpServerState::Connected => "connected",
        McpServerState::Failed => "failed",
        McpServerState::Disabled => "disabled",
    }
}

/// system/init（键序固定，本工程不写的键见 headless.md D1）。
pub(crate) fn init(init: &Init<'_>, uuid: &str) -> Value {
    let slash: Vec<&str> = init
        .catalog
        .skills
        .iter()
        .map(String::as_str)
        .chain(LOCAL_COMMANDS)
        .collect();
    json!({
        "type": "system",
        "subtype": "init",
        "cwd": init.cwd,
        "session_id": init.session_id,
        "tools": init.catalog.tools.iter().map(|name| tool_name(name)).collect::<Vec<_>>(),
        "mcp_servers": init
            .mcp_servers
            .iter()
            .map(|server| json!({"name": server.name, "status": mcp_status(server.state)}))
            .collect::<Vec<_>>(),
        "model": init.model,
        "permissionMode": init.permission_mode,
        "slash_commands": slash,
        "apiKeySource": init.api_key_source,
        "zcode_version": init.version,
        "output_style": "default",
        "agents": init.catalog.agents,
        "skills": init.catalog.skills,
        "uuid": uuid,
    })
}

fn usage_value(usage: Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        "cache_read_input_tokens": usage.cache_read_input_tokens,
        "output_tokens": usage.output_tokens,
    })
}

fn image_source(source: &ImageSource) -> Value {
    match source {
        ImageSource::Base64 { media_type, data } => {
            json!({"type": "base64", "media_type": media_type, "data": data})
        }
        // 大图在会话里是 Artifact 引用（README §6 规则 10），stream-json 不物化，原样写出引用。
        ImageSource::Artifact { .. } => serde_json::to_value(source).unwrap_or(Value::Null),
    }
}

fn result_content(item: &ToolResultContent) -> Value {
    match item {
        ToolResultContent::Text { text } => json!({"type": "text", "text": text}),
        ToolResultContent::Image { source } => {
            json!({"type": "image", "source": image_source(source)})
        }
        ToolResultContent::Document { source } => {
            json!({"type": "document", "source": image_source(source)})
        }
        ToolResultContent::ToolReference { tool_name } => {
            json!({"type": "tool_reference", "tool_name": tool_name})
        }
    }
}

/// 内容块的写法（headless.md §3.4、§3.5）。工具结果：成功 `{tool_use_id, type, content}`，失败
/// `{type, content, is_error, tool_use_id}`；只有一段文字时 `content` 是字符串。
pub(crate) fn block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Image { source } => json!({"type": "image", "source": image_source(source)}),
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            let mut value = json!({"type": "thinking", "thinking": thinking});
            if let Some(signature) = signature {
                value["signature"] = json!(signature);
            }
            value
        }
        ContentBlock::RedactedThinking { data } => {
            json!({"type": "redacted_thinking", "data": data})
        }
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id.as_str(), "name": name, "input": input})
        }
        ContentBlock::ServerToolUse { id, name, input } => {
            json!({"type": "server_tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::WebSearchToolResult {
            tool_use_id,
            content,
        } => {
            json!({"type": "web_search_tool_result", "tool_use_id": tool_use_id, "content": content})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let content = match content.as_slice() {
                [ToolResultContent::Text { text }] => json!(text),
                items => Value::Array(items.iter().map(result_content).collect()),
            };
            if *is_error {
                json!({"type": "tool_result", "content": content, "is_error": true,
                    "tool_use_id": tool_use_id.as_str()})
            } else {
                json!({"tool_use_id": tool_use_id.as_str(), "type": "tool_result", "content": content})
            }
        }
    }
}

/// 一条 assistant 消息的事件：每个内容块一个（流式时每个块一条消息，headless.md §3.4）。`stamp` 给每个事件
/// 一个新的 uuid 与时间。
pub(crate) fn assistant_events(
    message: &AssistantMessage,
    session_id: &str,
    mut stamp: impl FnMut() -> Stamp,
) -> Vec<Value> {
    message
        .content
        .iter()
        .map(|item| {
            let Stamp { uuid, timestamp } = stamp();
            json!({
                "type": "assistant",
                "message": {
                    "id": message.id.as_str(),
                    "type": "message",
                    "role": "assistant",
                    "model": message.model,
                    "content": [block(item)],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": usage_value(message.usage.unwrap_or_default()),
                    "context_management": null,
                },
                "parent_tool_use_id": null,
                "session_id": session_id,
                "uuid": uuid,
                "timestamp": timestamp,
            })
        })
        .collect()
}

/// 一条 user 消息的事件（headless.md §3.5）：meta 消息与压缩摘要另带 `isSynthetic`。
pub(crate) fn user_event(message: &UserMessage, session_id: &str, stamp: Stamp) -> Value {
    let mut event = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": message.content.iter().map(block).collect::<Vec<_>>(),
        },
        "parent_tool_use_id": null,
        "session_id": session_id,
        "uuid": stamp.uuid,
        "timestamp": stamp.timestamp,
    });
    if matches!(
        message.origin,
        UserOrigin::Meta { .. } | UserOrigin::CompactSummary
    ) {
        event["isSynthetic"] = json!(true);
    }
    event
}

/// 本地命令的合成 assistant（`/usage` 等，headless.md §3.6）。
pub(crate) fn synthetic_assistant(text: &str, session_id: &str, stamp: Stamp) -> Value {
    json!({
        "type": "assistant",
        "message": {
            "id": stamp.uuid,
            "type": "message",
            "role": "assistant",
            "model": "<synthetic>",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": usage_value(Usage::default()),
            "context_management": null,
        },
        "parent_tool_use_id": null,
        "session_id": session_id,
        "uuid": stamp.uuid,
        "timestamp": stamp.timestamp,
    })
}

/// result 的种类（headless.md §3.6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResultKind {
    /// `success`；回合失败时 `is_error` 为真，`result` 是错误文字。
    Success {
        is_error: bool,
        result: String,
        ttft_ms: Option<u64>,
    },
    /// 发请求前到了阻塞线（compact.md §2.2）：`success`、`is_error`，`result` 是错误文字，`terminal_reason` 是
    /// `blocking_limit`。
    Blocked { result: String },
    /// `error_max_turns`。
    MaxTurns { max: u64 },
    /// `error_during_execution`（被中断）。
    Interrupted,
}

impl ResultKind {
    pub(crate) fn is_error(&self) -> bool {
        match self {
            Self::Success { is_error, .. } => *is_error,
            Self::Blocked { .. } | Self::MaxTurns { .. } | Self::Interrupted => true,
        }
    }

    fn terminal_reason(&self) -> &'static str {
        match self {
            Self::Success {
                is_error: false, ..
            } => "completed",
            Self::Success { is_error: true, .. } => "api_error",
            Self::Blocked { .. } => "blocking_limit",
            Self::MaxTurns { .. } => "max_turns",
            Self::Interrupted => "aborted_streaming",
        }
    }
}

/// 被中断的回合的诊断（`error_during_execution` 的 `errors`，固定原文）。
pub(crate) const INTERRUPTED_DIAGNOSTIC: &str =
    "[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=null";

/// 一个回合的 result 的内容。
pub(crate) struct TurnResult<'a> {
    pub(crate) duration_api_ms: u64,
    pub(crate) stop_reason: Option<StopReason>,
    pub(crate) session_id: &'a str,
    pub(crate) usage: Usage,
    pub(crate) model_usage: &'a BTreeMap<String, Usage>,
    pub(crate) num_turns: u64,
    pub(crate) kind: ResultKind,
    pub(crate) duration_ms: u64,
    pub(crate) uuid: String,
    pub(crate) result_index: usize,
}

fn model_usage(models: &BTreeMap<String, Usage>) -> Value {
    let mut out = Map::new();
    for (model, usage) in models {
        out.insert(
            model.clone(),
            json!({
                "inputTokens": usage.input_tokens,
                "outputTokens": usage.output_tokens,
                "cacheReadInputTokens": usage.cache_read_input_tokens,
                "cacheCreationInputTokens": usage.cache_creation_input_tokens,
                "costUSD": 0,
            }),
        );
    }
    Value::Object(out)
}

/// 回合的 result（键序：公共部分、`is_error`、`num_turns`、种类的部分，最后 `type`、
/// `duration_ms`、`uuid`，再接回合的输入元数据）。
pub(crate) fn turn_result(result: TurnResult<'_>) -> Value {
    let mut map = Map::new();
    let mut put = |key: &str, value: Value| {
        map.insert(key.to_owned(), value);
    };
    put("duration_api_ms", json!(result.duration_api_ms));
    put("stop_reason", json!(result.stop_reason));
    put("session_id", json!(result.session_id));
    put("total_cost_usd", json!(0));
    put("usage", usage_value(result.usage));
    put("modelUsage", model_usage(result.model_usage));
    put("permission_denials", json!([]));
    put("terminal_reason", json!(result.kind.terminal_reason()));
    put("is_error", json!(result.kind.is_error()));
    put("num_turns", json!(result.num_turns));
    match &result.kind {
        ResultKind::Success {
            is_error,
            result: text,
            ttft_ms,
        } => {
            put("subtype", json!("success"));
            put("api_error_status", Value::Null);
            put("result", json!(text));
            if let (false, Some(ttft)) = (is_error, ttft_ms) {
                put("ttft_ms", json!(ttft));
            }
        }
        ResultKind::Blocked { result: text } => {
            put("subtype", json!("success"));
            put("api_error_status", Value::Null);
            put("result", json!(text));
        }
        ResultKind::MaxTurns { max } => {
            put("subtype", json!("error_max_turns"));
            put(
                "errors",
                json!([format!("Reached maximum number of turns ({max})")]),
            );
        }
        ResultKind::Interrupted => {
            put("subtype", json!("error_during_execution"));
            put("errors", json!([INTERRUPTED_DIAGNOSTIC]));
        }
    }
    put("type", json!("result"));
    put("duration_ms", json!(result.duration_ms));
    put("uuid", json!(result.uuid));
    put("queued_turn_count", json!(0));
    put("result_index", json!(result.result_index));
    Value::Object(map)
}

/// 本地命令（`/usage`、`/context`、`/rename`）的 result（本地命令的键序，headless.md §3.6）。
pub(crate) struct LocalResult<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) duration_api_ms: u64,
    pub(crate) model_usage: &'a BTreeMap<String, Usage>,
    pub(crate) result: &'a str,
    pub(crate) command: &'a str,
    pub(crate) duration_ms: u64,
    pub(crate) uuid: String,
    pub(crate) result_index: usize,
}

pub(crate) fn local_result(result: LocalResult<'_>) -> Value {
    json!({
        "is_error": false,
        "duration_api_ms": result.duration_api_ms,
        "num_turns": 0,
        "stop_reason": null,
        "session_id": result.session_id,
        "total_cost_usd": 0,
        "usage": usage_value(Usage::default()),
        "modelUsage": model_usage(result.model_usage),
        "permission_denials": [],
        "subtype": "success",
        "result": result.result,
        "local_command": result.command,
        "type": "result",
        "duration_ms": result.duration_ms,
        "uuid": result.uuid,
        "queued_turn_count": 0,
        "result_index": result.result_index,
    })
}
