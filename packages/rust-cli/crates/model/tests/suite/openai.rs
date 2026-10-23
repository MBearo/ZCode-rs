//! OpenAI Chat / Responses 经本地 HTTP 夹具的端到端：URL、鉴权 header、Provider header 与补丁、
//! SSE 任意切分后的事件，以及错误分类。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数与替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::Arc;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::message::{ContentBlock, StopReason};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, QuerySource, ToolDefinition};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_model::{FixedEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::json;

use super::common::{assemble, clock, collect, fast_retry, request};
use super::fixture::{Fixture, Reply};

const KEY: &str = "sk-openai-test-0123456789";

fn endpoint(protocol: WireProtocol, base_url: String) -> ModelEndpoint {
    ModelEndpoint {
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        protocol,
        base_url,
        model: String::new(),
        credential: Credential::ApiKey(RedactedString::new(KEY)),
        headers: vec![("X-Provider".to_owned(), RedactedString::new("p1"))],
        body_patches: vec![json!({"reasoning_effort": "high"})],
        mid_conversation_system: false,
        supports_image: true,
        renewable: false,
    }
}

async fn model(endpoint: ModelEndpoint) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    ProviderModel::new(egress, Arc::new(FixedEndpoint::new(endpoint)), clock())
        .with_retry(fast_retry(2))
}

/// 每个事件一行 `data:`，再把整段切成 7 字节的块（任意切分）。
fn sse(events: &[serde_json::Value], done: bool) -> Reply {
    let mut text: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    if done {
        text.push_str("data: [DONE]\n\n");
    }
    let mut reply = Reply::new(200).header("content-type", "text/event-stream");
    for chunk in text.as_bytes().chunks(7) {
        reply = reply.send(chunk.to_vec());
    }
    reply
}

fn read_tool() -> ToolDefinition {
    ToolDefinition {
        name: "Read".to_owned(),
        description: "Read".to_owned(),
        input_schema: json!({"type": "object"}),
        cache_control: None,
    }
}

#[tokio::test]
async fn chat_completions_stream_text_and_tool_calls() {
    let reply = sse(
        &[
            json!({"id": "c1", "model": "glm-5", "choices": [{"delta": {"reasoning_content": "hm"}}]}),
            json!({"choices": [{"delta": {"content": "Reading."}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "Read", "arguments": "{\"file_path\":"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"a.rs\"}"}}]}, "finish_reason": "tool_calls"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 9, "completion_tokens": 4}}),
        ],
        true,
    );
    let server = Fixture::start(vec![reply]).await;
    let model = model(endpoint(
        WireProtocol::OpenAiChatCompletions,
        format!("{}/api/paas/v4/", server.url()),
    ))
    .await;
    let mut request = request("read a.rs", QuerySource::MainThread);
    request.tools = vec![read_tool()];
    let events = collect(&model, request).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        assemble(&events),
        vec![
            ContentBlock::Thinking {
                thinking: "hm".to_owned(),
                signature: None,
            },
            ContentBlock::Text {
                text: "Reading.".to_owned(),
            },
            ContentBlock::ToolUse {
                id: rust_cli_kernel_api::ids::ToolUseId::new("call_1"),
                name: "Read".to_owned(),
                input: json!({"file_path": "a.rs"}),
            },
        ]
    );
    assert!(events.contains(&Ok(ModelEvent::MessageStop)));
    let captured = &server.requests()[0];
    assert_eq!(captured.target, "/api/paas/v4/chat/completions");
    assert_eq!(
        captured.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(captured.header("x-provider"), Some("p1"));
    assert_eq!(captured.header("accept"), Some("text/event-stream"));
    assert_eq!(captured.header("x-api-key"), None);
    assert_eq!(captured.header("anthropic-beta"), None);
    assert_eq!(
        captured.body,
        concat!(
            r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"read a.rs"}],"#,
            r#""tools":[{"type":"function","function":{"name":"Read","description":"Read","parameters":{"type":"object"}}}],"#,
            r#""tool_choice":"auto","stream":true,"stream_options":{"include_usage":true},"max_tokens":1024,"reasoning_effort":"high"}"#,
        )
    );
}

#[tokio::test]
async fn responses_stream_text_and_finish() {
    let reply = sse(
        &[
            json!({"type": "response.created", "response": {"id": "r1", "model": "gpt-5.4"}}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "m1", "type": "message"}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "item_id": "m1", "delta": "Hello"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"id": "m1", "type": "message"}}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 5, "output_tokens": 1}}}),
        ],
        false,
    );
    let server = Fixture::start(vec![reply]).await;
    let model = model(endpoint(
        WireProtocol::OpenAiResponses,
        format!("{}/v1", server.url()),
    ))
    .await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        assemble(&events),
        vec![ContentBlock::Text {
            text: "Hello".to_owned(),
        }]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            ..
        })
    )));
    let captured = &server.requests()[0];
    assert_eq!(captured.target, "/v1/responses");
    assert!(
        captured
            .body
            .starts_with(r#"{"model":"claude-sonnet-4-5","stream":true,"store":false,"#),
        "{}",
        captured.body
    );
    assert!(
        captured.body.ends_with(r#""reasoning_effort":"high"}"#),
        "{}",
        captured.body
    );
}

#[tokio::test]
async fn openai_errors_are_classified_and_retried() {
    let limited = Reply::new(429)
        .header("retry-after-ms", "1")
        .send(json!({"error": {"message": "slow", "type": "requests"}}).to_string());
    let too_long = Reply::new(400).send(
        json!({"error": {"message": "maximum context length exceeded", "code": "context_length_exceeded"}})
            .to_string(),
    );
    let server = Fixture::start(vec![limited, too_long]).await;
    let model = model(endpoint(WireProtocol::OpenAiChatCompletions, server.url())).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.as_slice(), [Err(ModelError::PromptTooLong { .. })]),
        "{events:?}"
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn a_chat_stream_without_done_is_a_protocol_error() {
    let reply = sse(
        &[json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "stop"}]})],
        false,
    );
    let server = Fixture::start(vec![reply]).await;
    let model = model(endpoint(WireProtocol::OpenAiChatCompletions, server.url())).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.last(), Some(Err(ModelError::Protocol { .. }))),
        "{events:?}"
    );
}

/// 模型不支持图片（model.md §10.5）：发出的请求里图片换成 Node 的占位文字；支持时照常是 `image_url`。
#[tokio::test]
async fn text_only_models_get_image_placeholders() {
    use rust_cli_kernel_api::message::ImageSource;
    use rust_cli_kernel_api::model::ApiContent;

    let done = || {
        sse(
            &[json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]})],
            true,
        )
    };
    let server = Fixture::start(vec![done(), done()]).await;
    let mut request = request("what is this", QuerySource::MainThread);
    request.messages[0].content.push(ApiContent {
        block: ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "iVBORw0KGgo=".to_owned(),
            },
        },
        cache_control: None,
    });
    for supports_image in [false, true] {
        let endpoint = ModelEndpoint {
            supports_pdf: false,
            supports_video: false,
            supports_audio: false,
            supports_image,
            ..endpoint(WireProtocol::OpenAiChatCompletions, server.url())
        };
        let events = collect(&model(endpoint).await, request.clone()).await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
    }
    let requests = server.requests();
    assert!(
        requests[0].body.contains(
            r#"{"type":"text","text":"[Attached image/png]\n[Media omitted from provider request because the selected model does not support image input.]"}"#
        ),
        "{}",
        requests[0].body
    );
    assert!(!requests[0].body.contains("image_url"));
    assert!(
        requests[1].body.contains(
            r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}}"#
        ),
        "{}",
        requests[1].body
    );
}
