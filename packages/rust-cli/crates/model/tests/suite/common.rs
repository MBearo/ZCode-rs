//! 场景测试共用的构造：SSE 文本、模型、请求，以及把事件流拼回内容块。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest,
    ModelSelection, QuerySource, RequestOptions,
};
use rust_cli_model::{AnthropicEndpoint, ProviderModel, RetryPolicy};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub const API_KEY: &str = "sk-ant-test-0123456789abcdefghij";

/// 一个 SSE 事件的文本。
pub fn event(data: Value) -> String {
    let name = data["type"].as_str().unwrap_or("message").to_owned();
    format!("event: {name}\ndata: {data}\n\n")
}

pub fn message_start() -> String {
    event(json!({"type": "message_start", "message": {
        "id": "msg_01", "type": "message", "role": "assistant", "model": "claude-sonnet-4-5",
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 25, "cache_creation_input_tokens": 0,
                  "cache_read_input_tokens": 0, "output_tokens": 1}}}))
}

pub fn block_start(index: usize, block: Value) -> String {
    event(json!({"type": "content_block_start", "index": index, "content_block": block}))
}

pub fn delta(index: usize, delta: Value) -> String {
    event(json!({"type": "content_block_delta", "index": index, "delta": delta}))
}

pub fn block_stop(index: usize) -> String {
    event(json!({"type": "content_block_stop", "index": index}))
}

pub fn message_end(stop_reason: &str, output_tokens: u64) -> String {
    let delta = event(json!({"type": "message_delta",
        "delta": {"stop_reason": stop_reason, "stop_sequence": null},
        "usage": {"output_tokens": output_tokens}}));
    format!("{delta}{}", event(json!({"type": "message_stop"})))
}

/// 一段完整的纯文本响应。
pub fn text_response(parts: &[&str]) -> String {
    let mut stream = message_start();
    stream.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    stream.push_str(&event(json!({"type": "ping"})));
    for part in parts {
        stream.push_str(&delta(0, json!({"type": "text_delta", "text": part})));
    }
    stream.push_str(&block_stop(0));
    stream.push_str(&message_end("end_turn", 9));
    stream
}

/// 测试用的快速重试：不抖动，等待几毫秒。
pub fn fast_retry(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        base_delay: Duration::from_millis(1),
        max_backoff: Duration::from_millis(4),
        jitter_ratio: 0.0,
        ..RetryPolicy::default()
    }
}

/// 注入给模型的时钟的“现在”（2020-09-13T12:26:40Z）：Retry-After 的 HTTP 日期按它换算（F7）。
pub const NOW_MS: u64 = 1_600_000_000_000;

pub struct TestClock;

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        NOW_MS
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

pub fn clock() -> Arc<dyn Clock> {
    Arc::new(TestClock)
}

pub async fn model_at(base_url: String, retry: RetryPolicy) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let endpoint = AnthropicEndpoint {
        base_url,
        api_key: RedactedString::new(API_KEY),
    };
    ProviderModel::anthropic(egress, endpoint, clock()).with_retry(retry)
}

pub fn request(prompt: &str, source: QuerySource) -> ModelRequest {
    ModelRequest {
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        system: Vec::new(),
        messages: vec![ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::Text {
                    text: prompt.to_owned(),
                },
                cache_control: None,
            }],
            output_config: None,
        }],
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: 1024,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source,
        options: RequestOptions::default(),
    }
}

pub async fn collect(
    model: &ProviderModel,
    request: ModelRequest,
) -> Vec<Result<ModelEvent, ModelError>> {
    model
        .stream(request, CancellationToken::new())
        .collect()
        .await
}

/// 把事件流拼回内容块（主循环要做的事）：文本与 thinking 累加，tool 输入的 JSON 片段连接后解析，
/// 空串等于 `{}`。
pub fn assemble(events: &[Result<ModelEvent, ModelError>]) -> Vec<ContentBlock> {
    let mut blocks: Vec<(usize, ContentBlock, String)> = Vec::new();
    for event in events {
        match event {
            Ok(ModelEvent::BlockStart { index, block }) => {
                blocks.push((*index, block.clone(), String::new()));
            }
            Ok(ModelEvent::BlockDelta { index, delta }) => {
                let (_, block, json) = blocks.iter_mut().find(|(at, ..)| at == index).unwrap();
                match (block, delta) {
                    (ContentBlock::Text { text }, BlockDelta::Text(more)) => text.push_str(more),
                    (ContentBlock::Thinking { thinking, .. }, BlockDelta::Thinking(more)) => {
                        thinking.push_str(more);
                    }
                    (ContentBlock::Thinking { signature, .. }, BlockDelta::Signature(value)) => {
                        *signature = Some(value.clone());
                    }
                    (ContentBlock::ToolUse { .. }, BlockDelta::InputJson(more)) => {
                        json.push_str(more)
                    }
                    (block, delta) => panic!("{delta:?} for {block:?}"),
                }
            }
            Ok(ModelEvent::BlockStop { index }) => {
                let (_, block, json) = blocks.iter_mut().find(|(at, ..)| at == index).unwrap();
                if let ContentBlock::ToolUse { input, .. } = block {
                    *input = if json.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(json).unwrap()
                    };
                }
            }
            _ => {}
        }
    }
    blocks.into_iter().map(|(_, block, _)| block).collect()
}
