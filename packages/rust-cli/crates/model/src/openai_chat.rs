//! OpenAI Chat Completions 协议（docs/specs/rust-cli/model.md §10.1）。
//!
//! 请求体与 Node 的 Provider 适配器（`apps/zcode-cli-rust/crates/model/src/model_protocol.rs`，对齐 Node）
//! 相同的键序：
//!
//! ```text
//! model, messages, tools?, tool_choice?, stream, stream_options, max_tokens, temperature?
//! ```
//!
//! 之后合并 Provider 的 option map 补丁（推理参数、`max_completion_tokens` 等）。补丁给出
//! `max_completion_tokens` 时去掉默认的 `max_tokens`，两者不同时发送（同一预算只发一次）。

use std::time::SystemTime;

use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::model::{ModelError, ModelRequest, ToolChoice, ToolDefinition};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint};
use rust_cli_net::{HeaderValue, HttpRequest, ResponseHeaders, header};
use serde_json::{Map, Value, json};

use crate::anthropic::HEADER_TIMEOUT;
use crate::endpoint::chat_completions_url;
use crate::openai_chat_stream::ChatStream;
use crate::openai_lower::{Item, Part, items};
use crate::protocol::{Failure, Protocol, StreamDecoder, bearer};
use crate::{openai_error, patch};

pub(crate) struct OpenAiChat;

impl Protocol for OpenAiChat {
    fn encode(
        &self,
        endpoint: &ModelEndpoint,
        request: ModelRequest,
        _profile: &crate::profiles::Profile,
    ) -> Result<HttpRequest, ModelError> {
        let body = body(&endpoint.model, request, &endpoint.body_patches)?;
        let mut http = HttpRequest::post(chat_completions_url(&endpoint.base_url), body)
            .header(header::ACCEPT, header::EVENT_STREAM)
            .header(header::CONTENT_TYPE, header::JSON);
        http.timeout = Some(HEADER_TIMEOUT);
        Ok(http)
    }

    fn credential_headers(&self, credential: &Credential) -> Vec<(String, HeaderValue)> {
        bearer(credential)
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(ChatStream::default())
    }

    fn classify(
        &self,
        status: u16,
        headers: &ResponseHeaders,
        body: &str,
        now: SystemTime,
    ) -> Failure {
        openai_error::from_response(status, headers, body, now)
    }
}

/// 编码请求体。消费请求：文本移入请求体，不复制历史。
pub(crate) fn body(
    model: &str,
    request: ModelRequest,
    patches: &[Value],
) -> Result<String, ModelError> {
    // OpenAI 协议没有 Anthropic 的服务端工具（WebSearch，model.md §10）。
    if !request.options.server_tools.is_empty() {
        return Err(ModelError::Unsupported {
            what: "server tools (web_search) on the OpenAI protocols".to_owned(),
        });
    }
    let ModelRequest {
        system,
        messages,
        tools,
        tool_choice,
        max_tokens,
        temperature,
        ..
    } = request;
    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.to_owned()));
    let messages = items(system, messages)?.into_iter().map(message).collect();
    body.insert("messages".into(), Value::Array(messages));
    if !tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(tools.into_iter().map(tool).collect()),
        );
        // Node SDK 有工具时显式发送默认的 auto。
        let choice = tool_choice.map_or_else(|| Value::String("auto".to_owned()), self::choice);
        body.insert("tool_choice".into(), choice);
    }
    body.insert("stream".into(), Value::Bool(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    body.insert("max_tokens".into(), Value::from(max_tokens));
    if let Some(temperature) = temperature {
        body.insert(
            "temperature".into(),
            crate::anthropic_request::number(temperature)?,
        );
    }
    let mut body = Value::Object(body);
    patch::apply(&mut body, patches);
    if let Value::Object(object) = &mut body
        && object.contains_key("max_completion_tokens")
    {
        object.shift_remove("max_tokens");
    }
    Ok(stringify(&body))
}

fn message(item: Item) -> Value {
    match item {
        Item::System(text) => json!({"role": "system", "content": text}),
        Item::User(parts) => {
            let content = match <[Part; 1]>::try_from(parts) {
                // 只有一段文本时发字符串（AI SDK openai-compatible 的写法），兼容只认字符串的网关。
                Ok([Part::Text(text)]) => Value::String(text),
                Ok([part]) => Value::Array(vec![user_part(part)]),
                Err(parts) => Value::Array(parts.into_iter().map(user_part).collect()),
            };
            let mut value = json!({"role": "user"});
            value["content"] = content;
            value
        }
        Item::Assistant {
            text,
            reasoning,
            calls,
        } => {
            let mut value = Map::new();
            value.insert("role".into(), Value::String("assistant".to_owned()));
            // 没有正文的工具调用写 null（Node 适配器的写法）。
            let content = if text.is_empty() && !calls.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            };
            value.insert("content".into(), content);
            if !reasoning.is_empty() {
                value.insert("reasoning_content".into(), Value::String(reasoning));
            }
            if !calls.is_empty() {
                let calls = calls
                    .into_iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": call.arguments},
                        })
                    })
                    .collect();
                value.insert("tool_calls".into(), Value::Array(calls));
            }
            Value::Object(value)
        }
        Item::ToolResult { call_id, output } => {
            json!({"role": "tool", "tool_call_id": call_id, "content": output})
        }
    }
}

/// OpenAI 的文件输入要文件名；document 块没有，取 AI SDK 的缺省值（model.md §10.6）。
pub(crate) const PDF_NAME: &str = "document.pdf";

fn user_part(part: Part) -> Value {
    match part {
        Part::Text(text) => json!({"type": "text", "text": text}),
        Part::Image(url) => json!({"type": "image_url", "image_url": {"url": url}}),
        // Node 经 AI SDK（`@ai-sdk/openai-compatible`）的写法；document 块没有文件名，AI SDK 也用这个缺省名。
        Part::File(url) => {
            json!({"type": "file", "file": {"filename": PDF_NAME, "file_data": url}})
        }
    }
}

fn tool(tool: ToolDefinition) -> Value {
    let mut function = Map::new();
    function.insert("name".into(), Value::String(tool.name));
    function.insert("description".into(), Value::String(tool.description));
    function.insert("parameters".into(), tool.input_schema);
    json!({"type": "function", "function": Value::Object(function)})
}

fn choice(choice: ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::Any => Value::String("required".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Tool { name } => json!({"type": "function", "function": {"name": name}}),
    }
}

#[cfg(test)]
#[path = "openai_chat_tests.rs"]
mod tests;
