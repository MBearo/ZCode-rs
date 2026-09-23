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
pub(super) fn body(
    config: &ModelConfig,
    mut messages: Vec<Value>,
    tools: &[Value],
) -> Result<Value, ModelFailure> {
    // 签名/加密推理绑定请求模型；跨模型续聊保留正文与工具，不能回放另一个模型的私有块。
    let foreign = |m: &Value| {
        m.get("_zcode_origin")
            .is_some_and(|o| o["provider"] != config.provider_id || o["model"] != config.model_id)
    };
    for message in &mut messages {
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
        ApiType::Anthropic => anthropic_body(config, &messages, tools)?,
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
            content.push(json!({"type":"tool_result","tool_use_id":message["tool_call_id"],"content":message["content"],"is_error":message["_zcode_tool_failed"].as_bool().unwrap_or(false)}));
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
    let tools=tools.iter().map(|t|json!({"name":t["function"]["name"],"description":t["function"]["description"],"input_schema":t["function"]["parameters"]})).collect::<Vec<_>>();
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
    pub fn new(api: ApiType) -> Self {
        match api {
            ApiType::Chat => Self::Chat(Default::default()),
            ApiType::Responses => Self::Responses(Default::default()),
            ApiType::Anthropic => Self::Anthropic(Default::default()),
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
mod tests {
    use super::*;
    #[test]
    fn migrated_media_and_foreign_reasoning_use_protocol_projection() {
        let mut config: ModelConfig = serde_json::from_value(json!({"providerId":"next","modelId":"same-id","reasoningLevel":"none","baseUrl":"https://example.invalid"})).unwrap();
        let history = vec![
            json!({"role":"user","content":[{"type":"text","text":"files"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aW1n"}},{"type":"file","file":{"filename":"a.pdf","file_data":"data:application/pdf;base64,cGRm"}}]}),
            json!({"role":"assistant","content":"preserved","reasoning_content":"private","_zcode_origin":{"provider":"previous","model":"same-id"},"_zcode_anthropic_thinking":[{"type":"thinking","signature":"private","thinking":"private"}],"_zcode_responses_reasoning":[{"type":"reasoning","id":"rs","encrypted_content":"private","summary":[]}],"tool_calls":[{"id":"call","function":{"name":"Read","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"call","content":"result"}),
        ];
        for api in [ApiType::Chat, ApiType::Responses, ApiType::Anthropic] {
            config.api_type = api;
            let body = body(&config, history.clone(), &[]).unwrap();
            assert!(!body.to_string().contains("private"));
            assert!(body.to_string().contains("preserved"));
            assert!(body.to_string().contains("call"));
            if api == ApiType::Responses {
                assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
                assert_eq!(body["input"][0]["content"][2]["type"], "input_file");
            }
            if api == ApiType::Anthropic {
                assert_eq!(body["messages"][0]["content"][1]["source"]["data"], "aW1n");
                assert_eq!(body["messages"][0]["content"][2]["type"], "document");
            }
        }
        assert_eq!(history[1]["reasoning_content"], "private");
    }
    #[test]
    fn anthropic_does_not_replay_reasoning_only_assistants() {
        let config: ModelConfig = serde_json::from_value(json!({"apiType":"anthropic-messages","providerId":"fixture","modelId":"fixture","reasoningLevel":"none","baseUrl":"https://example.invalid"})).unwrap();
        let messages = vec![
            json!({"role":"user","content":"task"}),
            json!({"role":"assistant","content":"", "reasoning_content":"thought", "_zcode_anthropic_thinking":[{"type":"thinking","thinking":"thought","signature":"signature"}]}),
            json!({"role":"user","content":"continue"}),
        ];
        let body = body(&config, messages.clone(), &[]).unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(
            messages[1]["_zcode_anthropic_thinking"][0]["signature"],
            "signature"
        );
    }
    #[test]
    fn anthropic_gateway_url_matches_ts_adapter() {
        for (base, expected) in [
            (
                "https://example.invalid",
                "https://example.invalid/v1/messages",
            ),
            (
                "https://example.invalid/gateway/",
                "https://example.invalid/gateway/v1/messages",
            ),
            (
                "https://example.invalid/V1//",
                "https://example.invalid/V1/messages",
            ),
        ] {
            assert_eq!(ApiType::Anthropic.url(base), expected);
        }
        assert_eq!(
            ApiType::Responses.url("https://example.invalid/api/"),
            "https://example.invalid/api/responses"
        );
        assert_eq!(
            ApiType::Chat.url("https://example.invalid/v1"),
            "https://example.invalid/v1/chat/completions"
        );
    }
    #[test]
    fn serializers_preserve_tool_associations_and_private_reasoning() {
        let mut config: ModelConfig = serde_json::from_value(json!({"providerId":"fixture","modelId":"fixture","reasoningLevel":"none","baseUrl":"https://example.invalid"})).unwrap();
        let messages = vec![
            json!({"role":"system","content":"system"}),
            json!({"role":"assistant","content":"answer","reasoning_content":"thought","tool_calls":[{"id":"call","function":{"name":"Read","arguments":"{}"}}],"_zcode_responses_reasoning":[{"type":"reasoning","id":"rs","summary":[],"encrypted_content":"opaque"}],"_zcode_anthropic_thinking":[{"type":"thinking","thinking":"thought","signature":"signature"},{"type":"redacted_thinking","data":"redacted"}]}),
            json!({"role":"tool","tool_call_id":"call","content":"failed","_zcode_tool_failed":true}),
            json!({"role":"user","content":"follow up"}),
        ];
        let chat = body(&config, messages.clone(), &[]).unwrap();
        assert_eq!(chat["messages"][1]["reasoning_content"], "thought");
        assert!(!chat.to_string().contains("_zcode_"));
        assert!(!chat.to_string().contains("opaque"));
        config.api_type = ApiType::Responses;
        let response = body(&config, messages.clone(), &[]).unwrap();
        assert_eq!(response["input"][1]["encrypted_content"], "opaque");
        assert_eq!(response["input"][3]["call_id"], "call");
        assert_eq!(response["input"][4]["call_id"], "call");
        assert!(!response.to_string().contains("signature"));
        config.api_type = ApiType::Anthropic;
        let anthropic = body(&config, messages.clone(), &[]).unwrap();
        assert_eq!(anthropic["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            anthropic["messages"][0]["content"][0]["signature"],
            "signature"
        );
        assert_eq!(anthropic["messages"][0]["content"][1]["data"], "redacted");
        assert_eq!(
            anthropic["messages"][1]["content"][0]["tool_use_id"],
            "call"
        );
        assert_eq!(anthropic["messages"][1]["content"][0]["is_error"], true);
        assert_eq!(anthropic["messages"][1]["content"][1]["text"], "follow up");
        assert!(anthropic.get("tools").is_none());
        assert!(!anthropic.to_string().contains("opaque"));
    }
}
