//! 每请求取端点、401 后续期凭据、连续过载后切 fallback 模型、重试通知。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数与替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use rust_cli_base::models;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{
    ModelError, ModelNotice, ModelPort, ModelSelection, ModelStreamItem, QuerySource,
};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, ModelEndpoints, WireProtocol};
use rust_cli_model::ProviderModel;
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{clock, collect, fast_retry, model_at, request, text_response};
use super::fixture::{Fixture, Reply};

/// 账号模型的替身：每次取端点给出下一个令牌，并记下被拒的令牌。
struct Rotating {
    base_url: String,
    issued: Mutex<u32>,
    rejected: Mutex<Vec<String>>,
}

#[async_trait]
impl ModelEndpoints for Rotating {
    async fn endpoint(
        &self,
        selection: &ModelSelection,
        rejected: Option<&ModelEndpoint>,
    ) -> Result<ModelEndpoint, ModelError> {
        if let Some(Credential::ApiKey(key)) = rejected.map(|endpoint| &endpoint.credential) {
            self.rejected.lock().unwrap().push(key.expose().to_owned());
        }
        let mut issued = self.issued.lock().unwrap();
        *issued += 1;
        Ok(ModelEndpoint {
            supports_pdf: false,
            supports_video: false,
            supports_audio: false,
            protocol: WireProtocol::AnthropicMessages,
            base_url: self.base_url.clone(),
            model: format!("{}-api", selection.model),
            credential: Credential::ApiKey(RedactedString::new(format!("token-{issued}"))),
            headers: Vec::new(),
            body_patches: Vec::new(),
            mid_conversation_system: true,
            supports_image: true,
            renewable: true,
        })
    }
}

fn error(status: u16, kind: &str) -> Reply {
    Reply::new(status)
        .send(json!({"type": "error", "error": {"type": kind, "message": kind}}).to_string())
}

#[tokio::test]
async fn a_rejected_account_credential_is_renewed_once_per_failure() {
    let server = Fixture::start(vec![
        error(401, "authentication_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let endpoints = Arc::new(Rotating {
        base_url: server.url(),
        issued: Mutex::new(0),
        rejected: Mutex::new(Vec::new()),
    });
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let model = ProviderModel::new(egress, endpoints.clone(), clock()).with_retry(fast_retry(2));
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = server.requests();
    assert_eq!(requests[0].header("x-api-key"), Some("token-1"));
    assert_eq!(requests[1].header("x-api-key"), Some("token-2"));
    // 请求体按端点给的模型 ID 编码，续期不重新编码。
    assert!(
        requests[1]
            .body
            .starts_with(&format!(r#"{{"model":"{}-api","#, models::GLM_5_3))
    );
    assert_eq!(*endpoints.rejected.lock().unwrap(), ["token-1"]);
}

#[tokio::test]
async fn repeated_overload_switches_to_the_fallback_model() {
    let replies = (0..3).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(10)).await;
    let mut with_fallback = request("hi", QuerySource::MainThread);
    with_fallback.options.fallback_model = Some(models::GLM_5_3_FLASH.to_owned());
    let events = collect(&model, with_fallback).await;
    assert_eq!(
        events,
        vec![Err(ModelError::FallbackTriggered {
            original_model: models::GLM_5_3.to_owned(),
            fallback_model: models::GLM_5_3_FLASH.to_owned(),
        })]
    );
    assert_eq!(server.requests().len(), 3);

    // 没有 fallback 时照常按重试上限重试。
    let replies = (0..4).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(matches!(
        events.as_slice(),
        [Err(ModelError::Overloaded { .. })]
    ));
    assert_eq!(server.requests().len(), 4);
}

/// F10：opus 系列的主模型没有 fallback 时，第 3 次过载后不再重试，报 `Repeated 529 Overloaded
/// errors`（连续过载次数达到上限）；其他系列照常按重试上限重试（上一个测试）。
#[tokio::test]
async fn opus_models_stop_after_three_overloads_without_a_fallback() {
    let replies = (0..4).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(10)).await;
    let mut opus = request("hi", QuerySource::MainThread);
    opus.selection.model = models::OPUS_5_5.to_owned();
    let events = collect(&model, opus).await;
    assert_eq!(
        events,
        vec![Err(ModelError::Overloaded {
            message: "Repeated 529 Overloaded errors".to_owned(),
        })]
    );
    assert_eq!(server.requests().len(), 3);
}

/// 设了 `IS_SANDBOX` 时不报 `Repeated 529 Overloaded errors`，照常按重试上限重试
/// （docs/specs/rust-cli/cli-args.md §9）。
#[tokio::test]
async fn a_sandbox_keeps_retrying_overloaded_opus_requests() {
    let replies = (0..4).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(3))
        .await
        .with_overload_retries_in_sandbox(true);
    let mut opus = request("hi", QuerySource::MainThread);
    opus.selection.model = models::OPUS_5_5.to_owned();
    let events = collect(&model, opus).await;
    assert!(matches!(
        events.as_slice(),
        [Err(ModelError::Overloaded { message })] if message != "Repeated 529 Overloaded errors"
    ));
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn retries_are_reported_as_notices() {
    let server = Fixture::start(vec![
        error(500, "api_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let items: Vec<_> = model
        .stream_with_notices(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await;
    match items.first() {
        Some(Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
            attempt: 1,
            max_retries: 3,
            error: ModelError::Server { status: 500, .. },
            ..
        }))) => {}
        other => panic!("expected a retry notice, got {other:?}"),
    }
    // 重发拿到响应头时清掉重试通知（L35，stall-visibility.md §2.4）。
    assert!(matches!(
        items.get(1),
        Some(Ok(ModelStreamItem::Notice(ModelNotice::Resumed)))
    ));
    assert!(
        items[2..]
            .iter()
            .all(|item| matches!(item, Ok(ModelStreamItem::Event(_))))
    );
    // `stream` 不带通知。
    let server = Fixture::start(vec![
        error(500, "api_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
}
