//! 智谱的端点（docs/specs/rust-cli/provider-errors.md §5.3，修复线 F113）：码表经夹具的端到端。夹具在 127.0.0.1，按
//! 只给测试的声明 `with_assumed_base_url` 把它当成智谱的地址查厂商（D13）。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::Arc;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, QuerySource};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_model::{FixedEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

use super::common::{clock, collect, fast_retry, message_start, model_at, request, text_response};
use super::fixture::{Fixture, Reply};

const ZHIPU_ANTHROPIC: &str = "https://open.bigmodel.cn/api/anthropic";
const ZHIPU_OPENAI: &str = "https://open.bigmodel.cn/api/paas/v4";

async fn zhipu(server: &Fixture) -> ProviderModel {
    model_at(server.url(), fast_retry(3))
        .await
        .with_assumed_base_url(ZHIPU_ANTHROPIC)
}

fn coded(status: u16, code: &str, message: &str) -> Reply {
    Reply::new(status)
        .header("content-type", "application/json")
        .send(json!({"error": {"code": code, "message": message}}).to_string())
}

/// HTTP 429 带配额码：不重试，配额用尽（以前按 429 重试到上限）。别的端点上同样的回复照旧按限流重试。
#[tokio::test]
async fn a_quota_code_on_429_is_not_retried() {
    let server = Fixture::start(vec![coded(429, "1308", "已达到套餐用量上限")]).await;
    let events = collect(
        &zhipu(&server).await,
        request("hi", QuerySource::MainThread),
    )
    .await;
    assert_eq!(
        events,
        vec![Err(ModelError::QuotaExhausted {
            message: "已达到套餐用量上限".to_owned()
        })]
    );
    assert_eq!(server.requests().len(), 1);

    let server = Fixture::start(vec![
        coded(429, "1308", "已达到套餐用量上限"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let generic = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&generic, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(server.requests().len(), 2);
}

/// 码在表里就按表，不看状态：401 带限流码按限流重试。
#[tokio::test]
async fn a_rate_limit_code_wins_over_the_status() {
    let server = Fixture::start(vec![
        coded(401, "1302", "并发数过高"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let events = collect(
        &zhipu(&server).await,
        request("hi", QuerySource::MainThread),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(server.requests().len(), 2);
}

/// HTTP 200 之后不带事件名的错误块（还没有正文）：按码表是限流，重发；第一次的 `message_start` 不交出。
#[tokio::test]
async fn a_rate_limit_block_in_the_stream_is_resent() {
    let mut first = message_start();
    first.push_str(&format!(
        "data: {}\n\n",
        json!({"error": {"code": "1302", "message": "并发数过高"}})
    ));
    let server = Fixture::start(vec![
        Reply::new(200).send(first),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let events = collect(
        &zhipu(&server).await,
        request("hi", QuerySource::MainThread),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Ok(ModelEvent::MessageStart { .. })))
            .count(),
        1
    );
    assert_eq!(server.requests().len(), 2);
}

fn chat_endpoint(base_url: String) -> ModelEndpoint {
    ModelEndpoint {
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        base_url,
        model: "glm-5".to_owned(),
        credential: Credential::ApiKey(RedactedString::new("sk-zhipu-test-0123456789")),
        headers: Vec::new(),
        body_patches: Vec::new(),
        mid_conversation_system: false,
        supports_image: true,
        renewable: false,
    }
}

async fn chat(server: &Fixture) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    ProviderModel::new(
        egress,
        Arc::new(FixedEndpoint::new(chat_endpoint(server.url()))),
        clock(),
    )
    .with_retry(fast_retry(3))
    .with_assumed_base_url(ZHIPU_OPENAI)
}

fn chat_reply(chunks: &[Value]) -> Reply {
    let mut text: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    text.push_str("data: [DONE]\n\n");
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(text)
}

fn chunk(content: &str, finish: Option<&str>) -> Value {
    json!({"id": "c1", "model": "glm-5",
        "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": finish}]})
}

/// OpenAI 协议的智谱端点：`sensitive` 是内容拦截的结束原因（以前是 `Unknown`，回合静默结束）。
#[tokio::test]
async fn sensitive_is_a_refusal_on_the_openai_protocol() {
    let server = Fixture::start(vec![chat_reply(&[
        chunk("Hel", None),
        chunk("lo", Some("sensitive")),
    ])])
    .await;
    let events = collect(&chat(&server).await, request("hi", QuerySource::MainThread)).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::Refusal),
                ..
            })
        )),
        "{events:?}"
    );
}

/// `network_error` 是流内的网络失败：还没有正文时重发；有正文之后交给调用方（以前截断的内容按正常完成）。
#[tokio::test]
async fn network_error_is_a_stream_failure() {
    let server = Fixture::start(vec![
        chat_reply(&[chunk("", Some("network_error"))]),
        chat_reply(&[chunk("ok", Some("stop"))]),
    ])
    .await;
    let events = collect(&chat(&server).await, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(server.requests().len(), 2);

    let server = Fixture::start(vec![chat_reply(&[
        chunk("partial", None),
        chunk("", Some("network_error")),
    ])])
    .await;
    let events = collect(&chat(&server).await, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.last(), Some(Err(ModelError::Network { .. }))),
        "{events:?}"
    );
    assert_eq!(server.requests().len(), 1);
}
