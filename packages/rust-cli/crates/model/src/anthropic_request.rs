//! Anthropic Messages 请求体（docs/specs/rust-cli/model.md §3）。
//!
//! 字段顺序：
//!
//! ```text
//! model, messages, system, tools, tool_choice?, metadata?, max_tokens, thinking?, temperature?,
//! context_management?, output_config?, diagnostics?, stream
//! ```
//!
//! 带 `?` 的字段为空时省略（规则见各字段的注释）。值按 JavaScript
//! `JSON.stringify` 输出（`rust_cli_base::js::json`），与 Node 发出的字节一致。`betas` 不进请求体，
//! 走 `anthropic-beta` header（`anthropic_betas`）。Provider 的 option map 补丁最后合并（`patch`）。

use rust_cli_base::js::json::{self, stringify};
use rust_cli_kernel_api::model::{
    CacheControl, ContextEdit, ModelError, ModelRequest, ServerTool, SystemBlock, ThinkingConfig,
    ToolChoice, ToolDefinition, ToolFlags,
};
use serde_json::{Map, Number, Value};

use crate::anthropic_betas::{self, MessageFlags, writes_diagnostics};
use crate::profiles::Profile;
use crate::{anthropic_write, patch};

/// 编码好的请求体与它要发的 `anthropic-beta`（model.md §3、§3.1）：`betas` 按请求用到的特性（一方），`node_betas` 照
/// Node（兼容端点，工作线 F64）；协议层按方案选一个。
pub(crate) struct Encoded {
    pub(crate) body: String,
    pub(crate) betas: Vec<String>,
    pub(crate) node_betas: Vec<String>,
}

/// 编码成请求体文本，见 [`encode`]。`first_party` 为真时按 Anthropic 官方方案，否则按“其他 Anthropic 兼容”
/// （这一层只差 `diagnostics`；system 形状与卡片字段在 `anthropic_shape` 里改）。
#[cfg(test)]
pub(crate) fn body(
    request: ModelRequest,
    first_party: bool,
    patches: &[Value],
) -> Result<String, ModelError> {
    encode(request, tests::profile(first_party), patches, &[]).map(|encoded| encoded.body)
}

/// 编码成请求体与 beta 列表。消费请求：顶层的小字段移入对象，消息直接写成文本，不复制历史。
///
/// `profile`：请求方案（决定是否写 `diagnostics`，model.md §3.2）；`patches`：Provider 的
/// option map 补丁；`explicit`：调用方显式要的 beta。
pub(crate) fn encode(
    request: ModelRequest,
    profile: &Profile,
    patches: &[Value],
    explicit: &[String],
) -> Result<Encoded, ModelError> {
    let ModelRequest {
        selection,
        system,
        messages,
        tools,
        tool_choice,
        max_tokens,
        thinking,
        temperature,
        betas: _,
        source,
        options,
    } = request;
    // 性能修复（修复线 F6，审查 R5-6）：消息是请求体的大头，从类型化消息直接写成文本（`anthropic_write`），不再先建
    // `serde_json::Value`；其余字段很小，仍建成对象以便合并补丁，写出时把消息文本放回 `messages` 的位置。
    let flags = MessageFlags::of(&messages);
    let mut messages_text = String::new();
    anthropic_write::messages(&mut messages_text, &messages)?;
    drop(messages);
    let mut body = Map::new();
    body.insert("model".into(), Value::String(selection.model));
    // 占位：补丁不碰这个键时它留在原位，写出时换成 `messages_text`。
    body.insert("messages".into(), Value::Null);
    body.insert(
        "system".into(),
        Value::Array(system.into_iter().map(system_block).collect()),
    );
    let mut flags_by_tool = options.tool_flags;
    let mut tools: Vec<Value> = tools
        .into_iter()
        .map(|item| {
            let flags = flags_by_tool.remove(&item.name).unwrap_or_default();
            tool(item, flags)
        })
        .collect();
    tools.extend(options.server_tools.into_iter().map(server_tool));
    body.insert("tools".into(), Value::Array(tools));
    if let Some(choice) = tool_choice {
        body.insert("tool_choice".into(), self::tool_choice(choice));
    }
    // 每个请求都应写 metadata（`{user_id}`）；这里在上层给出 user_id 时才写。
    if let Some(user_id) = options.user_id {
        let metadata = object([("user_id", Value::String(user_id))]);
        body.insert("metadata".into(), Value::Object(metadata));
    }
    body.insert("max_tokens".into(), Value::from(max_tokens));
    if let Some(thinking) = thinking {
        body.insert("thinking".into(), self::thinking(thinking));
    }
    if let Some(temperature) = temperature {
        body.insert("temperature".into(), number(temperature)?);
    }
    // 有上下文编辑就写，beta context-management 跟着加（anthropic_betas）。
    if !options.context_edits.is_empty() {
        let edits = options
            .context_edits
            .into_iter()
            .map(context_edit)
            .collect();
        let management = object([("edits", Value::Array(edits))]);
        body.insert("context_management".into(), Value::Object(management));
    }
    if let Some(config) = crate::anthropic_output::config(options.effort, options.output_format) {
        body.insert("output_config".into(), config);
    }
    // 会话第一次请求没有上一条响应，显式写 `previous_message_id: null`。
    if writes_diagnostics(profile.first_party_extras, &source) {
        let previous = options
            .previous_message_id
            .map_or(Value::Null, Value::String);
        let diagnostics = object([("previous_message_id", previous)]);
        body.insert("diagnostics".into(), Value::Object(diagnostics));
    }
    // 发送时在请求参数之后追加 `stream: true`（`{...params, stream: true}`）。
    body.insert("stream".into(), Value::Bool(true));
    let mut body = Value::Object(body);
    // 补丁显式改了 `messages`（或整个换掉请求体）：把写好的消息解析回值再合并，结果与以前相同；beta 按合并后的看。
    if patches
        .iter()
        .any(|patch| !patch.is_object() || patch.get("messages").is_some())
    {
        body["messages"] =
            serde_json::from_str(&messages_text).map_err(|error| ModelError::InvalidRequest {
                message: format!("encoding the messages: {error}"),
            })?;
        patch::apply(&mut body, patches);
        let flags = MessageFlags::of_values(&body["messages"]);
        return Ok(Encoded {
            betas: anthropic_betas::betas(&body, flags, explicit),
            node_betas: anthropic_betas::node_betas(&body, flags),
            body: stringify(&body),
        });
    }
    patch::apply(&mut body, patches);
    Ok(Encoded {
        betas: anthropic_betas::betas(&body, flags, explicit),
        node_betas: anthropic_betas::node_betas(&body, flags),
        body: write_body(&body, &messages_text),
    })
}

/// 按 `JSON.stringify` 写出请求体，`messages` 的占位换成写好的消息文本。
fn write_body(body: &Value, messages: &str) -> String {
    let Value::Object(fields) = body else {
        return stringify(body);
    };
    let mut out = String::with_capacity(messages.len() + 64 * 1024);
    out.push('{');
    for (index, (key, value)) in json::properties(fields).enumerate() {
        if index > 0 {
            out.push(',');
        }
        json::write_str(&mut out, key);
        out.push(':');
        if key == "messages" {
            out.push_str(messages);
        } else {
            json::write(&mut out, value);
        }
    }
    out.push('}');
    out
}

fn context_edit(edit: ContextEdit) -> Value {
    Value::Object(match edit {
        ContextEdit::ClearThinking { keep } => object([
            ("type", text("clear_thinking_20251015")),
            ("keep", Value::String(keep)),
        ]),
    })
}

fn object<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

/// Node 的 WebSearch 工具类型（`@ai-sdk/anthropic` 3.0.81）；beta 的判定按它认（F55：以前写了两次）。
pub(crate) const NODE_WEB_SEARCH: &str = "web_search_20260209";

/// 服务端工具的定义；没有的域名列表不写。修改（工作线 F93，用户决定 2026-10-24，model.md §3.2）：一律是 Node 的
/// `web_search_20260209`（`@ai-sdk/anthropic` 3.0.81：`max_uses` 在域名之前），F23 到 F93 之间官方地址用
/// `web_search_20250305`（域名在 `max_uses` 之前）。OpenAI 两种协议不走这里。
fn server_tool(tool: ServerTool) -> Value {
    match tool {
        ServerTool::WebSearch {
            allowed_domains,
            blocked_domains,
            max_uses,
        } => {
            let mut out = object([
                ("type", text(NODE_WEB_SEARCH)),
                ("name", text("web_search")),
                ("max_uses", Value::from(max_uses)),
            ]);
            if let Some(domains) = allowed_domains {
                out.insert("allowed_domains".into(), domains.into());
            }
            if let Some(domains) = blocked_domains {
                out.insert("blocked_domains".into(), domains.into());
            }
            Value::Object(out)
        }
    }
}

fn system_block(block: SystemBlock) -> Value {
    let mut value = object([("type", text("text")), ("text", Value::String(block.text))]);
    if let Some(cache) = block.cache_control {
        value.insert("cache_control".into(), cache_control(cache));
    }
    Value::Object(value)
}

/// 工具定义的键序：基本三项，`strict`、`eager_input_streaming`，再
/// `defer_loading`，最后缓存断点。
pub(crate) fn tool(tool: ToolDefinition, flags: ToolFlags) -> Value {
    let mut value = object([
        ("name", Value::String(tool.name)),
        ("description", Value::String(tool.description)),
        ("input_schema", tool.input_schema),
    ]);
    for (name, on) in [
        ("strict", flags.strict),
        ("eager_input_streaming", flags.eager_input_streaming),
        ("defer_loading", flags.defer_loading),
    ] {
        if on {
            value.insert(name.into(), Value::Bool(true));
        }
    }
    if let Some(cache) = tool.cache_control {
        value.insert("cache_control".into(), cache_control(cache));
    }
    Value::Object(value)
}

/// 缓存断点的键序：`type`，再按需 `ttl`、`scope`。
fn cache_control(cache: CacheControl) -> Value {
    let mut value = object([("type", Value::String(cache.kind))]);
    if let Some(ttl) = cache.ttl {
        value.insert("ttl".into(), Value::String(ttl));
    }
    if let Some(scope) = cache.scope {
        value.insert("scope".into(), Value::String(scope));
    }
    Value::Object(value)
}

fn tool_choice(choice: ToolChoice) -> Value {
    Value::Object(match choice {
        ToolChoice::Auto => object([("type", text("auto"))]),
        ToolChoice::Any => object([("type", text("any"))]),
        ToolChoice::None => object([("type", text("none"))]),
        ToolChoice::Tool { name } => {
            object([("type", text("tool")), ("name", Value::String(name))])
        }
    })
}

/// 构造 enabled 时先写 `budget_tokens` 再写 `type`。
fn thinking(thinking: ThinkingConfig) -> Value {
    Value::Object(match thinking {
        ThinkingConfig::Enabled { budget_tokens } => object([
            ("budget_tokens", Value::from(budget_tokens)),
            ("type", text("enabled")),
        ]),
        ThinkingConfig::Adaptive { display } => {
            let mut value = object([("type", text("adaptive"))]);
            if let Some(display) = display {
                value.insert("display".into(), Value::String(display));
            }
            value
        }
        ThinkingConfig::Disabled => object([("type", text("disabled"))]),
    })
}

/// f32 按最短十进制表示转成 JSON 数字（0.7 是 `0.7` 而不是 `0.699999988079071`；1 是 `1`）。
pub(crate) fn number(value: f32) -> Result<Value, ModelError> {
    format!("{value}")
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map(Value::Number)
        .ok_or_else(|| ModelError::InvalidRequest {
            message: format!("temperature must be finite, got {value}"),
        })
}

#[cfg(test)]
#[path = "anthropic_request_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "anthropic_options_tests.rs"]
mod options_tests;

#[cfg(test)]
#[path = "anthropic_request_bytes_tests.rs"]
mod bytes_tests;
