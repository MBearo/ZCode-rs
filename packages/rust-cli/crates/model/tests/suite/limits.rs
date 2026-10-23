//! `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：模型端口把每个请求的 `max_tokens` 压到不超过它；没设时
//! 原样发。

use rust_cli_kernel_api::model::QuerySource;
use serde_json::Value;

use super::common::{collect, fast_retry, model_at, request, text_response};
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
