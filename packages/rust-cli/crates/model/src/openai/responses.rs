//! OpenAI Responses 协议（docs/specs/rust-cli/model.md §10.2）。
//!
//! 请求体与 Node 的 Provider 适配器相同的键序：
//!
//! ```text
//! model, stream, store, include, max_output_tokens, input, tools?, tool_choice?, temperature?
//! ```
//!
//! `store: false`（不在服务端留存）；`include: ["reasoning.encrypted_content"]` 与 Node 一致。system 提示是
//! `input` 的第一项，推理模型（o1 / o3 / o4-mini / gpt-5，`gpt-5-chat` 除外）用 `developer` 角色。

use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::model::{ModelError, ModelRequest, ToolChoice, ToolDefinition};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint};
use rust_cli_net::{HeaderValue, HttpRequest};
use serde_json::{Map, Value, json};

use crate::endpoint::responses_url;
use crate::errors::Vendor;
use crate::openai::lower::{Item, Part, items};
use crate::openai::responses_stream::ResponsesStream;
use crate::patch;
use crate::protocol::{Protocol, StreamDecoder};

pub(crate) struct OpenAiResponses;

impl Protocol for OpenAiResponses {
    fn encode(
        &self,
        endpoint: &ModelEndpoint,
        request: ModelRequest,
        _profile: &crate::profiles::Profile,
    ) -> Result<HttpRequest, ModelError> {
        super::encode(endpoint, request, responses_url, body)
    }

    fn credential_headers(&self, credential: &Credential) -> Vec<(String, HeaderValue)> {
        super::bearer(credential)
    }

    fn decoder(&self, vendor: Vendor) -> Box<dyn StreamDecoder> {
        Box::new(ResponsesStream::new(vendor))
    }
}

/// OpenAI SDK `getOpenAILanguageModelCapabilities` 的推理模型判断（决定 system 用 developer 角色）。
fn reasoning_model(model: &str) -> bool {
    use rust_cli_base::models::{GPT_5_CHAT, OPENAI_REASONING_PREFIXES};
    OPENAI_REASONING_PREFIXES
        .iter()
        .any(|prefix| model.starts_with(prefix))
        && !model.starts_with(GPT_5_CHAT)
}

/// 编码请求体。消费请求：文本移入请求体，不复制历史。
pub(crate) fn body(
    model: &str,
    request: ModelRequest,
    patches: &[Value],
) -> Result<String, ModelError> {
    let ModelRequest {
        system,
        messages,
        tools,
        tool_choice,
        max_tokens,
        temperature,
        ..
    } = request;
    let system_role = if reasoning_model(model) {
        "developer"
    } else {
        "system"
    };
    let mut input = Vec::new();
    for item in items(system, messages)? {
        lower(item, system_role, &mut input);
    }
    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.to_owned()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    body.insert("max_output_tokens".into(), Value::from(max_tokens));
    body.insert("input".into(), Value::Array(input));
    if !tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(tools.into_iter().map(tool).collect()),
        );
        let choice = tool_choice.map_or_else(|| Value::String("auto".to_owned()), self::choice);
        body.insert("tool_choice".into(), choice);
    }
    if let Some(temperature) = temperature {
        body.insert("temperature".into(), crate::wire::number(temperature)?);
    }
    let mut body = Value::Object(body);
    patch::apply(&mut body, patches);
    Ok(stringify(&body))
}

fn lower(item: Item, system_role: &str, input: &mut Vec<Value>) {
    match item {
        Item::System(text) => input.push(json!({"role": system_role, "content": text})),
        Item::User(parts) => {
            let content: Vec<Value> = parts
                .into_iter()
                .map(|part| match part {
                    Part::Text(text) => json!({"type": "input_text", "text": text}),
                    Part::Image(url) => json!({"type": "input_image", "image_url": url}),
                    Part::File(url) => json!({"type": "input_file",
                        "filename": super::PDF_NAME, "file_data": url}),
                })
                .collect();
            input.push(json!({"role": "user", "content": content}));
        }
        // 推理正文不回放：Responses 的推理项要有 item ID 与加密内容，canonical 的 thinking 块没有。
        Item::Assistant { text, calls, .. } => {
            if !text.is_empty() {
                input.push(json!({
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                }));
            }
            for call in calls {
                input.push(json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                }));
            }
        }
        Item::ToolResult { call_id, output } => input.push(json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        })),
    }
}

fn tool(tool: ToolDefinition) -> Value {
    let mut value = Map::new();
    value.insert("type".into(), Value::String("function".to_owned()));
    value.insert("name".into(), Value::String(tool.name));
    value.insert("description".into(), Value::String(tool.description));
    value.insert("parameters".into(), tool.input_schema);
    value.insert("strict".into(), Value::Bool(false));
    Value::Object(value)
}

fn choice(choice: ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::Any => Value::String("required".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
    }
}

#[cfg(test)]
#[path = "responses_tests.rs"]
mod tests;
