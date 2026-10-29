//! 失败路径：错误分类、重试与 Retry-After、流中途出错、截断、取消。

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, QuerySource};
use rust_cli_model::ProviderModel;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{
    collect, delta, event, fast_retry, message_start, model_at, request, started_text,
    text_response,
};
use super::fixture::{Fixture, Reply};

fn error_reply(status: u16, kind: &str, message: &str) -> Reply {
    Reply::new(status)
        .header("content-type", "application/json")
        .send(json!({"type": "error", "error": {"type": kind, "message": message}}).to_string())
}

fn ok_reply() -> Reply {
    Reply::new(200).send(text_response(&["ok"]))
}

#[tokio::test]
async fn a_rate_limit_is_retried_after_the_requested_wait() {
    let limited = error_reply(429, "rate_limit_error", "slow down").header("retry-after-ms", "40");
    let server = Fixture::start(vec![limited, ok_reply()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let started = Instant::now();
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(started.elapsed() >= Duration::from_millis(40));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    // 重试发出的是同一个请求。
    assert_eq!(requests[0].body, requests[1].body);
}

#[tokio::test]
async fn retries_stop_after_the_limit() {
    let replies = (0..5)
        .map(|_| error_reply(500, "api_error", "boom"))
        .collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(
        events,
        vec![Err(ModelError::Server {
            status: 500,
            message: "boom".to_owned()
        })]
    );
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn overload_is_retried_only_when_the_user_waits() {
    let server = Fixture::start(vec![error_reply(529, "overloaded_error", "Overloaded")]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(
        &model,
        request("title", QuerySource::Side("title".to_owned())),
    )
    .await;
    assert_eq!(
        events,
        vec![Err(ModelError::Overloaded {
            message: "Overloaded".to_owned()
        })]
    );
    assert_eq!(server.requests().len(), 1);

    let server = Fixture::start(vec![
        error_reply(529, "overloaded_error", "Overloaded"),
        ok_reply(),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::Compact)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn non_retryable_failures_return_at_once() {
    let unauthorized = error_reply(401, "authentication_error", "invalid x-api-key")
        .header("x-should-retry", "false");
    let too_long = error_reply(
        400,
        "invalid_request_error",
        "prompt is too long: 210000 tokens > 200000 maximum",
    );
    let wait_too_long = error_reply(429, "rate_limit_error", "later").header("retry-after", "120");
    let server = Fixture::start(vec![unauthorized, too_long, wait_too_long]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let mut results = Vec::new();
    for _ in 0..3 {
        results.extend(collect(&model, request("hi", QuerySource::MainThread)).await);
    }
    assert_eq!(
        results,
        vec![
            Err(ModelError::Authentication {
                message: "invalid x-api-key".to_owned()
            }),
            Err(ModelError::PromptTooLong {
                message: "prompt is too long: 210000 tokens > 200000 maximum".to_owned()
            }),
            // 要求等待 120 秒，超过 60 秒的上限：不等，直接报错并带上服务端要求的等待。
            Err(ModelError::RateLimited {
                message: "later".to_owned(),
                retry_after_ms: Some(120_000)
            }),
        ]
    );
    assert_eq!(server.requests().len(), 3);
}

/// F7（审查 R2-11）：Retry-After 的 HTTP 日期按注入的时钟换算。以前读系统时间：这里的日期（时钟之后 120 秒）
/// 在真实时间里早已过去，会被当成“不用等”去重试。
#[tokio::test]
async fn an_http_date_retry_after_is_measured_from_the_injected_clock() {
    let later = error_reply(429, "rate_limit_error", "later")
        .header("retry-after", "Sun, 13 Sep 2020 12:28:40 GMT");
    let server = Fixture::start(vec![later]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let results = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(
        results,
        vec![Err(ModelError::RateLimited {
            message: "later".to_owned(),
            retry_after_ms: Some(120_000)
        })]
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn a_refused_connection_is_retried_then_reported() {
    let url = {
        let server = Fixture::start(Vec::new()).await;
        server.url()
    };
    let model = model_at(url, fast_retry(1)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.as_slice(), [Err(ModelError::Network { .. })]),
        "{events:?}"
    );
}

#[tokio::test]
async fn an_error_event_ends_the_stream_after_the_events_before_it() {
    let mut stream = message_start();
    stream.push_str(&event(json!({"type": "content_block_start", "index": 0,
                                   "content_block": {"type": "text", "text": ""}})));
    stream.push_str(&delta(0, json!({"type": "text_delta", "text": "partial"})));
    stream.push_str(&event(json!({"type": "error",
                                   "error": {"type": "overloaded_error", "message": "Overloaded"}})));
    stream.push_str(&text_response(&["never read"]));
    let server = Fixture::start(vec![Reply::new(200).send(stream), ok_reply()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(events[..3].iter().all(Result::is_ok));
    assert_eq!(
        events[3],
        Err(ModelError::Overloaded {
            message: "Overloaded".to_owned()
        })
    );
    // 流已经开始之后的错误不重试。
    assert_eq!(server.requests().len(), 1);
}

/// 已有正文之后流在 `message_stop` 之前断开：是协议错误，交给调用方（还没有正文时由模型适配器重发，见
/// `stream_retry.rs`）。
#[tokio::test]
async fn a_stream_cut_before_message_stop_is_a_protocol_error() {
    let server = Fixture::start(vec![Reply::new(200).send(started_text("par"))]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(matches!(events[0], Ok(ModelEvent::MessageStart { .. })));
    assert!(
        matches!(&events[3..], [Err(ModelError::Protocol { .. })]),
        "{events:?}"
    );
    assert_eq!(server.requests().len(), 1);
}

/// 取消立即结束流：读到一半、等响应头、退避等待时都一样，之后流不再产出任何项。
#[tokio::test]
async fn cancellation_ends_the_stream_at_once() {
    let stalled = Reply::new(200).send(started_text("par")).hang();
    let slow_head = Reply::new(200).delay_head(10_000);
    let backoff = error_reply(503, "api_error", "busy").header("retry-after", "30");
    let server = Fixture::start(vec![stalled, slow_head, backoff]).await;
    let model = model_at(server.url(), fast_retry(3)).await;

    let cancel = CancellationToken::new();
    let mut stream = model.stream(request("hi", QuerySource::MainThread), cancel.clone());
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelEvent::MessageStart { .. }))
    ));
    // 文本块的开始与正文（越过交出边界时一起交出）。
    for _ in 0..2 {
        assert!(matches!(stream.next().await, Some(Ok(_))));
    }
    assert_cancelled_soon(stream, cancel).await;

    for _ in 0..2 {
        let cancel = CancellationToken::new();
        let stream = model.stream(request("hi", QuerySource::MainThread), cancel.clone());
        assert_cancelled_soon(stream, cancel).await;
    }
    assert_eq!(server.requests().len(), 3);

    // 已经取消的令牌：不发请求。
    let cancel = CancellationToken::new();
    cancel.cancel();
    let events: Vec<_> = model
        .stream(request("hi", QuerySource::MainThread), cancel)
        .collect()
        .await;
    assert_eq!(events, vec![Err(ModelError::Cancelled)]);
    assert_eq!(server.requests().len(), 3);
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "测试辅助函数；失败即测试失败"
)]
async fn assert_cancelled_soon(
    mut stream: futures_util::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
    cancel: CancellationToken,
) {
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let item = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("the stream ends after cancellation");
    assert_eq!(item, Some(Err(ModelError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(stream.next().await, None);
    trigger.await.unwrap();
}

#[tokio::test]
async fn an_unconfigured_model_reports_authentication() {
    let model = ProviderModel::unconfigured("ZCODE_API_KEY is not set");
    let events: Vec<_> = model
        .stream(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await;
    assert_eq!(
        events,
        vec![Err(ModelError::Authentication {
            message: "ZCODE_API_KEY is not set".to_owned()
        })]
    );
}
