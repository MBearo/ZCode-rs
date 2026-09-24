use super::{
    config::ModelConfig,
    model_stream::{Assembly, TextBuffer},
};
use crate::contract::{ModelFailure, ModelOutput};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
pub enum ApiType {
    #[default]
    #[serde(rename = "openai-chat-completions")]
    Chat,
    #[serde(rename = "openai-responses")]
    Responses,
    #[serde(rename = "anthropic-messages")]
    Anthropic,
}
impl ApiType {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Chat => "chat/completions",
            Self::Responses => "responses",
            Self::Anthropic => "messages",
        }
    }
    pub fn url(self, base: &str) -> String {
        let base = base.trim().trim_end_matches('/');
        // 对齐 TS normalizeAnthropicBaseURL，网关根路径需要在 adapter 边界补 /v1。
        let version = if self == Self::Anthropic && !base.to_ascii_lowercase().ends_with("/v1") {
            "/v1"
        } else {
            ""
        };
        format!("{base}{version}/{}", self.endpoint())
    }
    pub fn reasoning_key(self, key: &str) -> bool {
        match self {
            Self::Chat => matches!(key, "reasoning_effort" | "thinking" | "enable_thinking"),
            Self::Responses => matches!(key, "reasoning" | "reasoning_effort"),
            Self::Anthropic => key == "thinking",
        }
    }
}
/// `metadata_user_id` is Anthropic's `metadata.user_id` (Node
/// `anthropic-request-metadata.ts`); option patches still apply after it.
pub(super) fn body(
    config: &ModelConfig,
    mut messages: Vec<Value>,
    tools: &[Value],
    metadata_user_id: Option<&str>,
) -> Result<Value, ModelFailure> {
    // 签名/加密推理绑定请求模型；跨模型续聊保留正文与工具，不能回放另一个模型的私有块。
    let foreign = |m: &Value| {
        m.get("_zcode_origin")
            .is_some_and(|o| o["provider"] != config.provider_id || o["model"] != config.model_id)
    };
    for message in &mut messages {
        // 浏览器环境上下文等只面向模型的改写在请求边界替换正文（压缩同样经过此处）。
        if let Some(request) = message
            .as_object_mut()
            .and_then(|m| m.remove("_zcode_request_content"))
        {
            message["content"] = request;
        }
        if foreign(message) {
            for key in [
                "reasoning_content",
                "_zcode_anthropic_thinking",
                "_zcode_responses_reasoning",
            ] {
                message.as_object_mut().unwrap().remove(key);
            }
        }
    }
    // 非 function 的工具是 provider 原生工具（WebSearch 的 web_search），只有 Anthropic 能编码。
    if tools.iter().any(|t| t["type"] != "function") {
        match config.api_type {
            ApiType::Chat => return Err(ModelFailure::new("native_search_chat", false)),
            ApiType::Responses => return Err(ModelFailure::new("native_search_responses", false)),
            ApiType::Anthropic => {}
        }
    }
    let mut body = match config.api_type {
        ApiType::Chat => {
            for message in &mut messages {
                if let Some(obj) = message.as_object_mut() {
                    obj.retain(|k, _| !k.starts_with("_zcode_"));
                }
            }
            let mut body = json!({"model":config.model_id,"stream":true,"stream_options":{"include_usage":true},"max_tokens":config.max_output_tokens,"tools":tools});
            body["messages"] = messages.into();
            body
        }
        ApiType::Responses => {
            let mut input = vec![];
            for message in &messages {
                if let Some(items) = message["_zcode_responses_reasoning"].as_array() {
                    input.extend_from_slice(items);
                }
                let role = message["role"].as_str().ok_or_else(ModelFailure::invalid)?;
                if role == "tool" {
                    input.push(json!({"type":"function_call_output","call_id":message["tool_call_id"],"output":message["content"]}));
                } else if message["content"].is_array() {
                    let content = super::model_media::responses(&message["content"], role)?;
                    if !content.is_empty() {
                        input.push(json!({"role":role,"content":content}));
                    }
                } else if message["content"].as_str().is_some_and(|s| !s.is_empty()) {
                    input.push(json!({"role":role,"content":message["content"]}));
                }
                if let Some(calls) = message["tool_calls"].as_array() {
                    for call in calls {
                        input.push(json!({"type":"function_call","call_id":call["id"],"name":call["function"]["name"],"arguments":call["function"]["arguments"]}));
                    }
                }
            }
            let tools=tools.iter().map(|t|json!({"type":"function","name":t["function"]["name"],"description":t["function"]["description"],"parameters":t["function"]["parameters"],"strict":false})).collect::<Vec<_>>();
            let mut body = json!({"model":config.model_id,"stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":config.max_output_tokens});
            body["input"] = input.into();
            body["tools"] = tools.into();
            body
        }
        ApiType::Anthropic => {
            let mut body = anthropic_body(config, &messages, tools)?;
            if let Some(user) = metadata_user_id {
                body["metadata"] = json!({"user_id": user});
            }
            body
        }
    };
    if tools.is_empty() {
        body.as_object_mut().unwrap().remove("tools");
    }
    for (key, value) in &config.reasoning_parameters {
        if config.api_type == ApiType::Responses && key == "reasoning_effort" {
            body["reasoning"] = json!({"effort":value});
        } else {
            body[key] = value.clone();
        }
    }
    for patch in &config.option_patches {
        crate::domain::option_map::merge_patch(&mut body, patch);
    }
    Ok(body)
}
fn anthropic_body(
    config: &ModelConfig,
    messages: &[Value],
    tools: &[Value],
) -> Result<Value, ModelFailure> {
    let mut system = vec![];
    let cache_system = messages
        .iter()
        .any(|m| m["role"] == "system" && m["_zcode_cache_control"].is_object());
    let mut output: Vec<Value> = vec![];
    for message in messages {
        let mut content = vec![];
        let role = message["role"].as_str().ok_or_else(ModelFailure::invalid)?;
        if role == "system" {
            let text = message["content"]
                .as_str()
                .ok_or_else(ModelFailure::invalid)?;
            let mut block = json!({"type":"text","text":text});
            if message["_zcode_cache_control"].is_object() {
                block["cache_control"] = message["_zcode_cache_control"].clone();
            }
            system.push(block);
            continue;
        }
        if let Some(blocks) = message["_zcode_anthropic_thinking"].as_array() {
            content.extend_from_slice(blocks);
        }
        if role == "tool" {
            let mut result = json!({"type":"tool_result","tool_use_id":message["tool_call_id"],"content":message["content"]});
            // AI SDK 只在失败时写 is_error。
            if message["_zcode_tool_failed"] == true {
                result["is_error"] = true.into();
            }
            content.push(result);
        } else if message["content"].is_array() {
            content.extend(super::model_media::anthropic(&message["content"])?);
        } else if message["content"].as_str().is_some_and(|s| !s.is_empty()) {
            content.push(json!({"type":"text","text":message["content"]}));
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let input: Value = serde_json::from_str(
                    call["function"]["arguments"]
                        .as_str()
                        .ok_or_else(ModelFailure::invalid)?,
                )
                .map_err(|_| ModelFailure::invalid())?;
                if !input.is_object() {
                    return Err(ModelFailure::invalid());
                }
                content.push(json!({"type":"tool_use","id":call["id"],"name":call["function"]["name"],"input":input}));
            }
        }
        let role = if role == "tool" { "user" } else { role };
        // 对齐 TS reasoning-history-normalization；仅推理截断仍保留 canonical，请求不回放孤立 thinking。
        if content.is_empty()
            || (role == "assistant"
                && content
                    .iter()
                    .all(|b| matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking"))))
        {
            continue;
        }
        if let Some(last) = output.last_mut().filter(|m| m["role"] == role) {
            last["content"].as_array_mut().unwrap().extend(content);
        } else {
            let mut message = json!({"role":role});
            message["content"] = content.into();
            output.push(message);
        }
    }
    let tools = tools
        .iter()
        .map(|t| match t["type"].as_str() {
            Some("function") => json!({"name":t["function"]["name"],"description":t["function"]["description"],"input_schema":t["function"]["parameters"]}),
            // provider 原生工具（web_search_20260209）原样发送。
            _ => t.clone(),
        })
        .collect::<Vec<_>>();
    let mut body =
        json!({"model":config.model_id,"stream":true,"max_tokens":config.max_output_tokens});
    body["system"] = if cache_system {
        system.into()
    } else {
        system
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
            .into()
    };
    body["messages"] = output.into();
    body["tools"] = tools.into();
    Ok(body)
}
pub(super) enum ProtocolStream {
    Chat(Assembly),
    Responses(super::responses_stream::Responses),
    Anthropic(super::anthropic_stream::Anthropic),
}
impl ProtocolStream {
    /// `server_tools`: the request carried provider-native tools, whose blocks
    /// the Anthropic stream then accepts and skips.
    pub fn new(api: ApiType, server_tools: bool) -> Self {
        match api {
            ApiType::Chat => Self::Chat(Default::default()),
            ApiType::Responses => Self::Responses(Default::default()),
            ApiType::Anthropic => {
                Self::Anthropic(super::anthropic_stream::Anthropic::new(server_tools))
            }
        }
    }
    pub fn done(&self) -> bool {
        match self {
            Self::Chat(s) => s.done,
            Self::Responses(s) => s.inner.done,
            Self::Anthropic(s) => s.inner.done,
        }
    }
    pub async fn consume(
        &mut self,
        data: &str,
        output: &mut TextBuffer<'_>,
    ) -> Result<(), ModelFailure> {
        match self {
            Self::Chat(s) => s.consume(data, output).await,
            Self::Responses(s) => s.consume(data, output).await,
            Self::Anthropic(s) => s.consume(data, output).await,
        }
    }
    pub fn finish(self) -> Result<ModelOutput, ModelFailure> {
        match self {
            Self::Chat(s) => s.finish(),
            Self::Responses(s) => s.finish(),
            Self::Anthropic(s) => s.finish(),
        }
    }
}
pub(super) async fn delta(
    inner: &mut Assembly,
    output: &mut TextBuffer<'_>,
    delta: Value,
) -> Result<(), ModelFailure> {
    inner
        .consume_value(&json!({"choices":[{"delta":delta}]}), output)
        .await
}
pub(super) fn index(value: &Value) -> Result<u64, ModelFailure> {
    value
        .as_u64()
        .filter(|i| *i < 128)
        .ok_or_else(ModelFailure::invalid)
}
pub(super) fn required(value: &Value) -> Result<&str, ModelFailure> {
    value.as_str().ok_or_else(ModelFailure::invalid)
}

#[cfg(test)]
#[path = "model_protocol_tests.rs"]
mod tests;
