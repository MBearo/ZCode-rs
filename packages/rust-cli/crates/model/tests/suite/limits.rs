//! `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：模型端口把每个请求的 `max_tokens` 压到不超过它；没设时
//! 原样发。请求交给 Provider 补丁的上限随取端点交给端点来源（工作线 F95，model.md §9）。

#![allow(
    clippy::unwrap_used,
    reason = "替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::model::{ModelError, ModelSelection, QuerySource};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, ModelEndpoints, WireProtocol};
use rust_cli_model::ProviderModel;
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

use super::common::{clock, collect, fast_retry, model_at, request, text_response};
use super::fixture::{Fixture, Reply};

fn sse() -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(text_response(&["ok"]))
}

async fn sent_max_tokens(limit: Option<u32>, asked: u32) -> Option<Value> {
    let server = Fixture::start(vec![sse()]).await;
    let model = model_at(server.url(), fast_retry(0))
        .await
        .with_max_output_tokens(limit);
    let mut request = request("hello", QuerySource::Side("web_search".to_owned()));
    request.max_tokens = asked;
    let events = collect(&model, request).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let body = serde_json::from_str::<Value>(&server.requests()[0].body).ok()?;
    Some(body["max_tokens"].clone())
}

#[tokio::test]
async fn the_output_limit_caps_every_request() {
    assert_eq!(sent_max_tokens(None, 128_000).await, Some(128_000.into()));
    assert_eq!(
        sent_max_tokens(Some(4096), 128_000).await,
        Some(4096.into())
    );
    // 请求自己的值更小时不变（`min(请求的值, 有效上限)`）。
    assert_eq!(sent_max_tokens(Some(4096), 1000).await, Some(1000.into()));
}

/// 目录模型的替身：`max_tokens` 补丁按交来的上限写（没有时是配置的 128000），记下每次交来的上限。
struct Patching {
    base_url: String,
    limits: Mutex<Vec<Option<u64>>>,
}

#[async_trait]
impl ModelEndpoints for Patching {
    async fn endpoint(
        &self,
        selection: &ModelSelection,
        rejected: Option<&ModelEndpoint>,
    ) -> Result<ModelEndpoint, ModelError> {
        self.endpoint_with_output_limit(selection, rejected, None)
            .await
    }

    async fn endpoint_with_output_limit(
        &self,
        selection: &ModelSelection,
        _rejected: Option<&ModelEndpoint>,
        output_limit: Option<u64>,
    ) -> Result<ModelEndpoint, ModelError> {
        self.limits.lock().unwrap().push(output_limit);
        Ok(ModelEndpoint {
            protocol: WireProtocol::AnthropicMessages,
            base_url: self.base_url.clone(),
            model: selection.model.clone(),
            credential: Credential::None,
            headers: Vec::new(),
            body_patches: vec![json!({"max_tokens": output_limit.unwrap_or(128_000).min(128_000)})],
            mid_conversation_system: true,
            supports_image: true,
            supports_pdf: true,
            supports_video: false,
            supports_audio: false,
            renewable: false,
        })
    }
}

/// 请求的 `provider_max_output_tokens` 原样交给端点来源，补丁按它写最终的 `max_tokens`；不给的请求交 None。
#[tokio::test]
async fn the_provider_output_limit_reaches_the_endpoint() {
    let server = Fixture::start(vec![sse(), sse()]).await;
    let endpoints = Arc::new(Patching {
        base_url: server.url(),
        limits: Mutex::new(Vec::new()),
    });
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let model = ProviderModel::new(egress, endpoints.clone(), clock()).with_retry(fast_retry(0));
    let mut side = request("search", QuerySource::Side("web_search_tool".to_owned()));
    side.max_tokens = 4096;
    side.options.provider_max_output_tokens = Some(4096);
    assert!(collect(&model, side).await.iter().all(Result::is_ok));
    let main = request("hello", QuerySource::MainThread);
    assert!(collect(&model, main).await.iter().all(Result::is_ok));
    assert_eq!(*endpoints.limits.lock().unwrap(), [Some(4096), None]);
    let sent: Vec<Value> = server
        .requests()
        .iter()
        .map(|request| serde_json::from_str::<Value>(&request.body).unwrap()["max_tokens"].clone())
        .collect();
    assert_eq!(sent, [json!(4096), json!(128_000)]);
}
