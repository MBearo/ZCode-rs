//! 流内重试（docs/specs/rust-cli/provider-errors.md §3.2，修复线 F113）：还没越过交出边界的失败由模型适配器按处置表重发，
//! 第一次响应扣住的事件不交出；越过之后照旧交给调用方（`scenarios.rs`）。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use futures_util::StreamExt;
use rust_cli_base::models;
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelNotice, ModelPort, ModelStreamItem, QuerySource,
};
use rust_cli_model::ProviderModel;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{
    block_start, event, fast_retry, message_start, model_at, request, text_response,
};
use super::fixture::{Fixture, Reply};

fn stream_reply(body: String) -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(body)
}

/// `message_start` 与一个还没有正文的文本块，之后来一个错误事件（都在交出边界之前）。
fn early_error(kind: &str, message: &str) -> Reply {
    let mut body = message_start();
    body.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    body.push_str(&event(
        json!({"type": "error", "error": {"type": kind, "message": message}}),
    ));
    stream_reply(body)
}

fn ok() -> Reply {
    stream_reply(text_response(&["ok"]))
}

/// 带通知的流的形状：通知写成 `retrying(<错误>, <等待>)` / `resumed`，事件写成 `event`。
async fn shape_of(
    model: &ProviderModel,
    source: QuerySource,
) -> (Vec<String>, Vec<Result<ModelStreamItem, ModelError>>) {
    let items: Vec<_> = model
        .stream_with_notices(request("hi", source), CancellationToken::new())
        .collect()
        .await;
    let shape = items
        .iter()
        .map(|item| match item {
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
                error, delay_ms, ..
            })) => {
                let delay = if *delay_ms == 0 { "0" } else { "wait" };
                format!("retrying({error}, {delay})")
            }
            Ok(ModelStreamItem::Notice(notice)) => format!("{notice:?}").to_lowercase(),
            Ok(ModelStreamItem::Event(_)) => "event".to_owned(),
            Err(error) => format!("error({error})"),
        })
        .collect();
    (shape, items)
}

/// 流里的过载、还没有正文：重发同一个请求，第一次响应的事件一个都不交出（以前交出 `message_start` 后以过载结束，
/// 主循环以错误结束回合）。
#[tokio::test]
async fn an_overload_before_any_content_is_resent() {
    let server = Fixture::start(vec![early_error("overloaded_error", "Overloaded"), ok()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, items) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape[..2],
        ["retrying(model overloaded: Overloaded, wait)", "resumed"],
        "{shape:?}"
    );
    // 第二次响应的 6 个事件（`text_response`：开始、块开始、正文、块结束、用量、结束）。
    assert_eq!(shape[2..], ["event"; 6], "{shape:?}");
    assert!(matches!(
        items[2],
        Ok(ModelStreamItem::Event(ModelEvent::MessageStart { .. }))
    ));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
}

/// 后台来源的流内过载不重发（同响应头之前，provider-errors.md D19）。
#[tokio::test]
async fn a_background_overload_in_the_stream_is_not_resent() {
    let server = Fixture::start(vec![early_error("overloaded_error", "Overloaded"), ok()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::Side("title".to_owned())).await;
    assert_eq!(shape, ["error(model overloaded: Overloaded)"]);
    assert_eq!(server.requests().len(), 1);
}

/// 流里的限流按总次数重试；配额用尽不重试。
#[tokio::test]
async fn rate_limits_are_retried_and_quota_is_not() {
    let server = Fixture::start(vec![
        early_error("rate_limit_error", "later"),
        early_error("rate_limit_error", "later"),
        ok(),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape
            .iter()
            .filter(|item| item.starts_with("retrying"))
            .count(),
        2
    );
    assert_eq!(shape.last().map(String::as_str), Some("event"));
    assert_eq!(server.requests().len(), 3);

    let server = Fixture::start(vec![early_error("billing_error", "no credit"), ok()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape,
        ["error(API Error: Credit balance or quota exhausted · no credit)"]
    );
    assert_eq!(server.requests().len(), 1);
}

/// 还没有正文时的服务端错误：不等待地再发一次（G，D3 暂定 (b)）；第二次照旧失败时交给调用方。
#[tokio::test]
async fn a_server_error_before_any_content_is_resent_once() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        early_error("api_error", "Internal server error"),
        ok(),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape,
        [
            "retrying(server error 500: Internal server error, 0)",
            "resumed",
            "error(server error 500: Internal server error)"
        ]
    );
    assert_eq!(server.requests().len(), 2);
}

/// 流在交出边界之前正常关闭、没有 `message_stop`（截断）：重试 1 次，再 G 一次，之后交给调用方（D17 暂定）。
#[tokio::test]
async fn a_cut_before_any_content_is_retried_then_resent() {
    let cut = || stream_reply(message_start());
    let server = Fixture::start(vec![cut(), cut(), cut(), ok()]).await;
    let model = model_at(server.url(), fast_retry(5)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    let truncated = "protocol error: stream ended before message_stop";
    assert_eq!(
        shape,
        [
            format!("retrying({truncated}, wait)"),
            "resumed".to_owned(),
            format!("retrying({truncated}, 0)"),
            "resumed".to_owned(),
            format!("error({truncated})"),
        ]
    );
    assert_eq!(server.requests().len(), 3);
}

/// 流内的过载与响应头之前的过载共用“3 次”计数：第 3 次有 fallback 模型时换模型。
#[tokio::test]
async fn overloads_in_the_head_and_the_stream_share_the_count() {
    let overloaded = || {
        Reply::new(529)
            .header("content-type", "application/json")
            .send(
                json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
                    .to_string(),
            )
    };
    let server = Fixture::start(vec![
        overloaded(),
        early_error("overloaded_error", "Overloaded"),
        overloaded(),
        ok(),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(5)).await;
    let mut with_fallback = request("hi", QuerySource::MainThread);
    with_fallback.options.fallback_model = Some(models::GLM_5_3_FLASH.to_owned());
    let events: Vec<_> = model
        .stream(with_fallback, CancellationToken::new())
        .collect()
        .await;
    assert_eq!(
        events,
        vec![Err(ModelError::FallbackTriggered {
            original_model: models::GLM_5_3.to_owned(),
            fallback_model: models::GLM_5_3_FLASH.to_owned(),
        })]
    );
    assert_eq!(server.requests().len(), 3);
}
