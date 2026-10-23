//! Anthropic 请求体里的 `messages`：从类型化消息直接写成 JSON 文本（docs/specs/rust-cli/model.md §3）。
//!
//! 性能修复（修复线 F6，审查 R5-6）：以前先把整个请求建成 `serde_json::Value`（每个块一个 IndexMap），再按
//! JS 规则写出、最后整棵释放；消息是请求体的大头（长会话里几 MB），这三步约占每步客户端 CPU 的五分之一。
//! 现在直接写：字段顺序与以前建对象的插入顺序相同，字符串、数字与工具输入（`Value`）用
//! `rust_cli_base::js::json` 的同一套写法，所以字节不变（`anthropic_request_bytes_tests.rs` 拿旧写法逐字节比）。

use rust_cli_base::js::json::{write, write_str};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
use rust_cli_kernel_api::model::{ApiContent, ApiMessage, ApiRole, CacheControl, ModelError};
use serde_json::Value;

/// `[消息…]`。
pub(crate) fn messages(out: &mut String, messages: &[ApiMessage]) -> Result<(), ModelError> {
    out.push('[');
    for (index, item) in messages.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        message(out, item)?;
    }
    out.push(']');
    Ok(())
}

/// 一条消息解析成值（count_tokens 的请求体、补丁改了 `messages` 时用；与直接写出的是同一段文字）。
pub(crate) fn message_value(item: &ApiMessage) -> Result<Value, ModelError> {
    let mut out = String::new();
    message(&mut out, item)?;
    serde_json::from_str(&out).map_err(|error| ModelError::InvalidRequest {
        message: format!("encoding a message: {error}"),
    })
}

/// 键名与冒号。
fn key(out: &mut String, name: &str, first: bool) {
    if !first {
        out.push(',');
    }
    write_str(out, name);
    out.push(':');
}

/// `{role, content, output_config?}`：每轮 effort（beta `per-turn-control-2026-07-01`）跟在 content 之后。
fn message(out: &mut String, item: &ApiMessage) -> Result<(), ModelError> {
    let role = match item.role {
        ApiRole::User => "user",
        ApiRole::Assistant => "assistant",
        // 对话中间的 system 消息（beta `mid-conversation-system-2026-04-07`）原样发出。
        ApiRole::System => "system",
    };
    out.push('{');
    key(out, "role", true);
    write_str(out, role);
    key(out, "content", false);
    out.push('[');
    for (index, content) in item.content.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        self::content(out, content)?;
    }
    out.push(']');
    if let Some(effort) = item
        .output_config
        .as_ref()
        .and_then(|config| config.effort.as_ref())
    {
        key(out, "output_config", false);
        out.push('{');
        key(out, "effort", true);
        write_str(out, effort);
        out.push('}');
    }
    out.push('}');
    Ok(())
}

/// 内容块，缓存断点放在块的字段之后（`{...block, cache_control}`）。
fn content(out: &mut String, content: &ApiContent) -> Result<(), ModelError> {
    out.push('{');
    block(out, &content.block)?;
    if let Some(cache) = &content.cache_control {
        key(out, "cache_control", false);
        cache_control(out, cache);
    }
    out.push('}');
    Ok(())
}

/// `"type":"…"`（块的第一个字段）。
fn kind(out: &mut String, value: &str) {
    key(out, "type", true);
    write_str(out, value);
}

/// 一个字符串字段。
fn text_field(out: &mut String, name: &str, value: &str) {
    key(out, name, false);
    write_str(out, value);
}

/// 块的字段（不含外层花括号）。
fn block(out: &mut String, block: &ContentBlock) -> Result<(), ModelError> {
    match block {
        ContentBlock::Text { text } => {
            kind(out, "text");
            text_field(out, "text", text);
        }
        ContentBlock::Image { source } => {
            kind(out, "image");
            key(out, "source", false);
            image_source(out, source)?;
        }
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            kind(out, "thinking");
            text_field(out, "thinking", thinking);
            if let Some(signature) = signature {
                text_field(out, "signature", signature);
            }
        }
        ContentBlock::RedactedThinking { data } => {
            kind(out, "redacted_thinking");
            text_field(out, "data", data);
        }
        ContentBlock::ToolUse { id, name, input } => {
            kind(out, "tool_use");
            text_field(out, "id", id.as_str());
            text_field(out, "name", name);
            key(out, "input", false);
            write(out, input);
        }
        // 服务端工具的块原样回放（docs/specs/rust-cli/tools-more.md §5）。
        ContentBlock::ServerToolUse { id, name, input } => {
            kind(out, "server_tool_use");
            text_field(out, "id", id);
            text_field(out, "name", name);
            key(out, "input", false);
            write(out, input);
        }
        ContentBlock::WebSearchToolResult {
            tool_use_id,
            content,
        } => {
            kind(out, "web_search_tool_result");
            text_field(out, "tool_use_id", tool_use_id);
            key(out, "content", false);
            write(out, content);
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            // 多数内置工具的结果块以 tool_use_id 开头；is_error 只在失败时写。
            key(out, "tool_use_id", true);
            write_str(out, tool_use_id.as_str());
            text_field(out, "type", "tool_result");
            key(out, "content", false);
            out.push('[');
            for (index, item) in content.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                tool_result_content(out, item)?;
            }
            out.push(']');
            if *is_error {
                key(out, "is_error", false);
                out.push_str("true");
            }
        }
    }
    Ok(())
}

fn tool_result_content(out: &mut String, content: &ToolResultContent) -> Result<(), ModelError> {
    out.push('{');
    match content {
        ToolResultContent::Text { text } => {
            kind(out, "text");
            text_field(out, "text", text);
        }
        ToolResultContent::Image { source } => {
            kind(out, "image");
            key(out, "source", false);
            image_source(out, source)?;
        }
        // Read 整读 PDF 的结果（docs/specs/rust-cli/model.md §10.6）：source 的写法与图片相同。
        ToolResultContent::Document { source } => {
            kind(out, "document");
            key(out, "source", false);
            image_source(out, source)?;
        }
        // ToolSearch 的结果（docs/specs/rust-cli/tools-more.md §3.6）。
        ToolResultContent::ToolReference { tool_name } => {
            kind(out, "tool_reference");
            text_field(out, "tool_name", tool_name);
        }
    }
    out.push('}');
    Ok(())
}

fn image_source(out: &mut String, source: &ImageSource) -> Result<(), ModelError> {
    match source {
        ImageSource::Base64 { media_type, data } => {
            out.push('{');
            kind(out, "base64");
            text_field(out, "media_type", media_type);
            text_field(out, "data", data);
            out.push('}');
            Ok(())
        }
        // 大图在发给模型之前由使用方物化成 base64（README §6 规则 10）；适配器拿不到 Artifact 存储。
        ImageSource::Artifact { .. } => Err(ModelError::InvalidRequest {
            message: "image artifacts must be materialized before the model request".to_owned(),
        }),
    }
}

/// 缓存断点的键序：`type`，再按需 `ttl`、`scope`。
fn cache_control(out: &mut String, cache: &CacheControl) {
    out.push('{');
    kind(out, &cache.kind);
    if let Some(ttl) = &cache.ttl {
        text_field(out, "ttl", ttl);
    }
    if let Some(scope) = &cache.scope {
        text_field(out, "scope", scope);
    }
    out.push('}');
}
