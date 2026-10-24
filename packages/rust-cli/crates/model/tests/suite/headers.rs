//! 模型请求的头（工作线 F64，model.md §2.4）：合并顺序、Provider 配置的覆盖与每请求头，经本地 HTTP 夹具看线上的头。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数与替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::ids::{AgentId, RunId, SessionId};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_model::{FixedEndpoint, ProviderModel, RequestHeaders, SourceFacts, source_headers};
use rust_cli_net::{Egress, NetworkPolicy};

use super::common::{clock, collect, fast_retry, request, text_response};
use super::fixture::{Captured, Fixture, Reply};

fn endpoint(base_url: String, headers: &[(&str, &str)]) -> ModelEndpoint {
    ModelEndpoint {
        protocol: WireProtocol::AnthropicMessages,
        base_url,
        model: "claude-opus-5-5".to_owned(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), RedactedString::new(*value)))
            .collect(),
        body_patches: Vec::new(),
        mid_conversation_system: true,
        supports_image: true,
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        renewable: false,
    }
}

fn facts() -> SourceFacts {
    SourceFacts {
        version: "0.1.52".to_owned(),
        endpoint_origin: "https://zcode.z.ai".to_owned(),
        release_channel: "production".to_owned(),
        locale: Some("en-US".to_owned()),
        time_zone: Some("Asia/Shanghai".to_owned()),
        platform: "darwin".to_owned(),
        arch: "arm64".to_owned(),
        os_release: Some("24.6.0".to_owned()),
    }
}

/// 来源头与 ID：trace 固定，`x-request-id` 依次是 `req-1`、`req-2` …
fn ids() -> RequestHeaders {
    let next = Arc::new(AtomicU32::new(0));
    RequestHeaders {
        source: source_headers(&facts()),
        trace_id: "trace-1".to_owned(),
        request_id: Arc::new(move || format!("req-{}", next.fetch_add(1, Ordering::SeqCst) + 1)),
    }
}

async fn model(endpoint: ModelEndpoint, retries: u32) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    ProviderModel::new(egress, Arc::new(FixedEndpoint::new(endpoint)), clock())
        .with_retry(fast_retry(retries))
        .with_request_headers(ids())
}

fn ok() -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(text_response(&["Hi"]))
}

/// 同名的头只有一个（名字不分大小写）。
fn single<'a>(captured: &'a Captured, name: &str) -> Option<&'a str> {
    let values: Vec<_> = captured
        .headers
        .iter()
        .filter(|(existing, _)| existing.eq_ignore_ascii_case(name))
        .collect();
    assert!(values.len() <= 1, "{name}: {:?}", captured.headers);
    values.first().map(|(_, value)| value.as_str())
}

/// 修 bug（F64）：Provider 配置的 `User-Agent` 覆盖本工程的（Node `mergeModelRequestHeaders`：名字不分大小写，后来的
/// 替换旧的）。以前 `user-agent` 在 Provider 配置之后加，配置的值被换成 `ZCode/<版本>`。
#[tokio::test]
async fn the_provider_user_agent_replaces_ours() {
    let server = Fixture::start(vec![ok()]).await;
    let model = model(endpoint(server.url(), &[("User-Agent", "ProbeUA/9")]), 0).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        single(&server.requests()[0], "user-agent"),
        Some("ProbeUA/9")
    );
}

/// 来源头照 Node 发；Provider 配置覆盖来源头与适配器的头（名字不分大小写），去掉配置里的 `x-zcode-query-source`，
/// 但不能覆盖每请求头；配置的 `anthropic-beta` 与算出的取并集。
#[tokio::test]
async fn provider_headers_go_between_the_source_and_the_request_headers() {
    let server = Fixture::start(vec![ok()]).await;
    let configured = [
        ("X-Title", "Probe"),
        ("Accept", "application/x-probe"),
        ("X-Request-Id", "static"),
        ("X-ZCode-Query-Source", "static"),
        ("Anthropic-Beta", " Context-1M-2025-08-07 ,, "),
        ("X-Probe", "p1"),
    ];
    let model = model(endpoint(server.url(), &configured), 0).await;
    let mut main = request("hi", QuerySource::MainThread);
    main.options.session_id = Some(SessionId::new("sess_s1"));
    main.messages.push(rust_cli_kernel_api::model::ApiMessage {
        role: rust_cli_kernel_api::model::ApiRole::System,
        content: main.messages[0].content.clone(),
        output_config: None,
    });
    let events = collect(&model, main).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let captured = &server.requests()[0];
    for (name, value) in [
        ("http-referer", "https://zcode.z.ai"),
        ("user-agent", "ZCode/0.1.52"),
        ("x-zcode-app-version", "0.1.52"),
        ("x-title", "Probe"),
        ("x-release-channel", "production"),
        ("x-client-language", "en-US"),
        ("x-client-timezone", "Asia/Shanghai"),
        ("x-zcode-agent", "glm"),
        ("x-platform", "darwin-arm64"),
        ("x-os-category", "macos"),
        ("x-os-version", "24.6.0"),
        ("accept", "application/x-probe"),
        ("x-probe", "p1"),
        ("x-request-id", "req-1"),
        ("x-zcode-trace-id", "trace-1"),
        ("x-zcode-query-source", "main_turn"),
        ("x-zcode-session-type", "main"),
        ("x-session-id", "s1"),
        (
            "anthropic-beta",
            "mid-conversation-system-2026-04-07,context-1m-2025-08-07",
        ),
    ] {
        assert_eq!(single(captured, name), Some(value), "{name}");
    }
    assert_eq!(single(captured, "x-query-id"), None);
}

/// 重试换新的 `x-request-id`，trace、回合与会话不变；子代理的请求是 `subagent`，会话与回合都去掉内部前缀。
#[tokio::test]
async fn each_attempt_gets_a_new_request_id() {
    let server = Fixture::start(vec![Reply::new(500).send("{}"), ok()]).await;
    let model = model(endpoint(server.url(), &[]), 1).await;
    let mut child = request("hi", QuerySource::Subagent("Explore".to_owned()));
    child.options.session_id = Some(SessionId::new("sess_subagent_agent_a1"));
    child.options.run = Some(RunId::new("run_r1"));
    child.options.agent = Some(AgentId::new("agent_a1"));
    let events = collect(&model, child).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for (index, captured) in requests.iter().enumerate() {
        let request_id = format!("req-{}", index + 1);
        for (name, value) in [
            ("x-request-id", request_id.as_str()),
            ("x-zcode-trace-id", "trace-1"),
            ("x-query-id", "r1"),
            ("x-zcode-query-source", "subagent"),
            ("x-zcode-session-type", "subagent"),
            ("x-session-id", "a1"),
        ] {
            assert_eq!(single(captured, name), Some(value), "{index} {name}");
        }
    }
}

/// 旁路请求用 Node 的用途名。工作线 F93 删掉计数请求之前，这里还核对计数只有 ID 与 `other`。
#[tokio::test]
async fn side_requests_use_the_node_query_source() {
    let server = Fixture::start(vec![ok()]).await;
    let model = model(endpoint(server.url(), &[]), 0).await;
    let mut title = request("hi", QuerySource::Side("generate_session_title".to_owned()));
    title.options.session_id = Some(SessionId::new("sess_s1"));
    title.options.run = Some(RunId::new("run_r2"));
    let events = collect(&model, title).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = server.requests();
    assert_eq!(
        single(&requests[0], "x-zcode-query-source"),
        Some("session_title")
    );
    assert_eq!(single(&requests[0], "x-zcode-session-type"), Some("main"));
    assert_eq!(single(&requests[0], "x-query-id"), Some("r2"));
}
