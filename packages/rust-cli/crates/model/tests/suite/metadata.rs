//! 工作线 F52（docs/specs/rust-cli/model.md §3.3，用户决定）：每个 messages 请求都带 `metadata.user_id`。
//! 值是设备身份（入口注入）加请求的会话；文本照 Node `createAnthropicRequestMetadataUserId`。
//! 工作线 F52b（用户决定 2026-10-22）：只有 Anthropic 协议带，OpenAI 两种协议不写、也不读设备身份（Node 相同）。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::FutureExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_model::{DeviceIdSource, FixedEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

use super::common::{clock, collect, fast_retry, request, text_response};
use super::fixture::{Fixture, Reply};

const DEVICE: &str = "7f1431f0-53e8-41c1-9cf9-22d11aa51d6a";
const SESSION: &str = "sess_2d480878-38af-4bae-a54f-096e2fb0f4d7";
/// Node `createAnthropicRequestMetadataUserId({deviceMid: DEVICE, sessionId: SESSION})` 的结果（golden 同
/// `tests/fixtures/metadata-golden.json` 的第一项）。
const USER_ID: &str = r#"{"device_id":"7f1431f0-53e8-41c1-9cf9-22d11aa51d6a","account_uuid":"","session_id":"2d480878-38af-4bae-a54f-096e2fb0f4d7"}"#;

fn device(calls: Arc<AtomicUsize>) -> DeviceIdSource {
    Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { DEVICE.to_owned() }.boxed()
    })
}

fn endpoint(protocol: WireProtocol, base_url: String) -> ModelEndpoint {
    ModelEndpoint {
        protocol,
        base_url,
        model: String::new(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: Vec::new(),
        body_patches: Vec::new(),
        mid_conversation_system: false,
        supports_image: true,
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        renewable: false,
    }
}

fn openai_reply() -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
}

fn responses_reply() -> Reply {
    let events = [
        json!({"type": "response.created", "response": {"id": "r1", "model": "m"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "m1", "type": "message"}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "item_id": "m1", "delta": "ok"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"id": "m1", "type": "message"}}),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 1, "output_tokens": 1}}}),
    ];
    let text: String = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(text)
}

/// 发一个请求，返回请求体与设备身份被读的次数。
async fn sent_body(protocol: WireProtocol, reply: Reply, session: Option<&str>) -> (Value, usize) {
    let server = Fixture::start(vec![reply]).await;
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let fixed = FixedEndpoint::new(endpoint(protocol, server.url()));
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ProviderModel::new(egress, Arc::new(fixed), clock())
        .with_retry(fast_retry(0))
        .with_device_id(device(calls.clone()));
    let mut request = request("hi", QuerySource::MainThread);
    request.options.session_id = session.map(SessionId::new);
    let events = collect(&model, request).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let body = serde_json::from_str(&server.requests()[0].body).unwrap();
    (body, calls.load(Ordering::SeqCst))
}

/// 修 bug：以前 Anthropic 请求没有 metadata（只在调用方给了 user_id 时写，没有调用方给）。
#[tokio::test]
async fn the_anthropic_protocol_sends_the_metadata_user_id() {
    let (anthropic, calls) = sent_body(
        WireProtocol::AnthropicMessages,
        Reply::new(200)
            .header("content-type", "text/event-stream")
            .send(text_response(&["ok"])),
        Some(SESSION),
    )
    .await;
    assert_eq!(anthropic["metadata"], json!({"user_id": USER_ID}));
    assert_eq!(calls, 1);
    // 键序：`tool_choice` 之后、`max_tokens` 之前（model.md §3）。
    let keys: Vec<&String> = anthropic.as_object().unwrap().keys().collect();
    let at = |name: &str| keys.iter().position(|key| *key == name).unwrap();
    assert!(
        at("tools") < at("metadata") && at("metadata") < at("max_tokens"),
        "{keys:?}"
    );
}

/// 工作线 F52b（用户决定：“算了，只给 anthropic 协议加吧”）：OpenAI Chat 与 Responses 的请求不写 `metadata`，也不读
/// 设备身份。修的问题：F52 三种协议都写，OpenAI 官方的 Chat Completions 在没有 `store: true` 时报 400。
#[tokio::test]
async fn the_openai_protocols_send_no_metadata() {
    for (protocol, reply) in [
        (WireProtocol::OpenAiChatCompletions, openai_reply()),
        (WireProtocol::OpenAiResponses, responses_reply()),
    ] {
        let (body, calls) = sent_body(protocol, reply, Some(SESSION)).await;
        assert_eq!(body.get("metadata"), None, "{protocol:?}: {body}");
        assert_eq!(calls, 0, "{protocol:?}");
    }
}

/// 请求没有带会话：`session_id` 是空字符串（Node 相同）。
#[tokio::test]
async fn a_request_without_a_session_has_an_empty_session_id() {
    let reply = Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(text_response(&["ok"]));
    let (body, _) = sent_body(WireProtocol::AnthropicMessages, reply, None).await;
    assert_eq!(
        body["metadata"]["user_id"],
        format!(r#"{{"device_id":"{DEVICE}","account_uuid":"","session_id":""}}"#)
    );
}

/// 调用方显式给了 user_id 时原样用，不取设备身份；没有设备身份的来源（测试）时不写 metadata。
#[tokio::test]
async fn an_explicit_user_id_wins_and_no_source_writes_nothing() {
    let server = Fixture::start(vec![
        Reply::new(200)
            .header("content-type", "text/event-stream")
            .send(text_response(&["ok"])),
        Reply::new(200)
            .header("content-type", "text/event-stream")
            .send(text_response(&["ok"])),
    ])
    .await;
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let fixed = || {
        Arc::new(FixedEndpoint::new(endpoint(
            WireProtocol::AnthropicMessages,
            server.url(),
        )))
    };
    let model = ProviderModel::new(egress.clone(), fixed(), clock())
        .with_retry(fast_retry(0))
        .with_device_id(device(calls.clone()));
    let mut request = request("hi", QuerySource::MainThread);
    request.options.user_id = Some("explicit".to_owned());
    collect(&model, request).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let plain = ProviderModel::new(egress, fixed(), clock()).with_retry(fast_retry(0));
    collect(&plain, request_with_session()).await;
    let bodies: Vec<Value> = server
        .requests()
        .iter()
        .map(|captured| serde_json::from_str(&captured.body).unwrap())
        .collect();
    assert_eq!(bodies[0]["metadata"], json!({"user_id": "explicit"}));
    assert_eq!(bodies[1].get("metadata"), None);
}

fn request_with_session() -> rust_cli_kernel_api::model::ModelRequest {
    let mut request = request("hi", QuerySource::Side("title".to_owned()));
    request.options.session_id = Some(SessionId::new(SESSION));
    request
}
