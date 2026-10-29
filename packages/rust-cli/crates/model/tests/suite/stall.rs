//! 流空闲看门狗（docs/specs/rust-cli/stall-visibility.md §2）：停顿通知、恢复、静默重发与 `StreamIdle`。
//! 上限在测试里调小（配置直接给 model；环境变量的规则在 cli 的单测）。连续 `ping` 只有前 30 个算数、停顿之后
//! 恢复要的是精确的时刻关系，在暂停的时钟上测（`src/transport_read_tests.rs`，修复线 F17）。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelNotice, ModelPort, ModelStreamItem, QuerySource,
};
use rust_cli_model::{ByteIdle, ProviderModel, StreamIdle};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{
    block_start, delta, fast_retry, message_start, model_at, request, started_text, text_response,
};
use super::fixture::{Fixture, Reply};

/// 字节级 `bytes` 毫秒、事件级 `events` 毫秒、停顿通知 50 ms。
fn idle(bytes: u64, events: u64) -> StreamIdle {
    StreamIdle {
        events: Some(Duration::from_millis(events)),
        bytes: Some(ByteIdle::Fixed(Duration::from_millis(bytes))),
        stall_notice: Duration::from_millis(50),
    }
}

async fn model(server: &Fixture, idle: StreamIdle) -> ProviderModel {
    model_at(server.url(), fast_retry(3))
        .await
        .with_stream_idle(idle)
}

async fn items(model: &ProviderModel) -> Vec<Result<ModelStreamItem, ModelError>> {
    model
        .stream_with_notices(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await
}

/// 通知的种类（`Retrying` 只看错误），事件写成 `event`。
fn shape(items: &[Result<ModelStreamItem, ModelError>]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
                error, delay_ms, ..
            })) => format!("retrying({error}, {delay_ms})"),
            Ok(ModelStreamItem::Notice(ModelNotice::Stalled { .. })) => "stalled".to_owned(),
            Ok(ModelStreamItem::Notice(ModelNotice::Resumed)) => "resumed".to_owned(),
            Ok(ModelStreamItem::Event(_)) => "event".to_owned(),
            Err(error) => format!("error({error:?})"),
        })
        .collect()
}

fn stream_reply() -> Reply {
    Reply::new(200).header("content-type", "text/event-stream")
}

#[tokio::test]
async fn a_stream_that_never_starts_is_resent_once_without_waiting() {
    let server = Fixture::start(vec![
        stream_reply().hang(),
        stream_reply().send(text_response(&["ok"])),
    ])
    .await;
    let items = items(&model(&server, idle(150, 10_000)).await).await;
    let shape = shape(&items);
    assert_eq!(
        shape[..3],
        [
            "stalled",
            "retrying(Stream idle timeout - no chunks received, 0)",
            "resumed"
        ],
        "{shape:?}"
    );
    assert!(shape[3..].iter().all(|item| item == "event"), "{shape:?}");
    match &items[1] {
        Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
            attempt: 1,
            max_retries: 3,
            error: ModelError::StreamIdle { idle_ms: 150 },
            ..
        })) => {}
        other => panic!("{other:?}"),
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
}

/// 第二次停顿：静默重发已经用过，走一次流式重发（provider-errors.md D3 暂定 (b)，修复线 F113：以前直接报
/// `StreamIdle`）；第三次才报错。
#[tokio::test]
async fn a_second_stall_before_any_event_is_resent_once_more() {
    let server = Fixture::start(vec![
        stream_reply().hang(),
        stream_reply().hang(),
        stream_reply().hang(),
    ])
    .await;
    let items = items(&model(&server, idle(150, 10_000)).await).await;
    assert_eq!(
        shape(&items),
        [
            "stalled",
            "retrying(Stream idle timeout - no chunks received, 0)",
            "resumed",
            "stalled",
            "retrying(Stream idle timeout - no chunks received, 0)",
            "resumed",
            "stalled",
            "error(StreamIdle { idle_ms: 150 })"
        ]
    );
    assert_eq!(server.requests().len(), 3);
}

/// 收到过事件、还没越过交出边界时停住：不静默重发，走一次流式重发（F113：以前报 `StreamIdle`）。
#[tokio::test]
async fn a_stall_before_any_content_is_resent_once() {
    let server = Fixture::start(vec![
        stream_reply().send(message_start()).hang(),
        stream_reply().send(text_response(&["ok"])),
    ])
    .await;
    let items = items(&model(&server, idle(150, 10_000)).await).await;
    let shape = shape(&items);
    assert_eq!(
        shape[..3],
        [
            "stalled",
            "retrying(Stream idle timeout - no chunks received, 0)",
            "resumed"
        ],
        "{shape:?}"
    );
    assert!(shape[3..].iter().all(|item| item == "event"), "{shape:?}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn a_stall_after_events_ends_with_stream_idle_without_resending() {
    let partial = format!(
        "{}{}{}",
        message_start(),
        block_start(0, json!({"type": "text", "text": ""})),
        delta(0, json!({"type": "text_delta", "text": "par"}))
    );
    let server = Fixture::start(vec![stream_reply().send(partial).hang()]).await;
    let items = items(&model(&server, idle(150, 10_000)).await).await;
    assert_eq!(
        shape(&items),
        [
            "event",
            "event",
            "event",
            "stalled",
            "error(StreamIdle { idle_ms: 150 })"
        ]
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn a_response_after_an_http_retry_resumes() {
    let server = Fixture::start(vec![
        Reply::new(500).send(r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#),
        stream_reply().send(text_response(&["ok"])),
    ])
    .await;
    let items = items(&model(&server, idle(1_000, 10_000)).await).await;
    let shape = shape(&items);
    assert!(shape[0].starts_with("retrying("), "{shape:?}");
    assert_eq!(shape[1], "resumed", "{shape:?}");
    assert!(shape[2..].iter().all(|item| item == "event"), "{shape:?}");
}

#[tokio::test]
async fn cancelling_while_stalled_ends_at_once() {
    let server = Fixture::start(vec![stream_reply().send(started_text("par")).hang()]).await;
    let model = model(&server, idle(10_000, 10_000)).await;
    let cancel = CancellationToken::new();
    let mut stream =
        model.stream_with_notices(request("hi", QuerySource::MainThread), cancel.clone());
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamItem::Event(ModelEvent::MessageStart { .. })))
    ));
    // 文本块的开始与正文。
    for _ in 0..2 {
        assert!(matches!(
            stream.next().await,
            Some(Ok(ModelStreamItem::Event(_)))
        ));
    }
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamItem::Notice(ModelNotice::Stalled { .. })))
    ));
    cancel.cancel();
    assert_eq!(stream.next().await, Some(Err(ModelError::Cancelled)));
    assert_eq!(stream.next().await, None);
}

#[tokio::test]
async fn the_plain_stream_hides_the_notices() {
    let server = Fixture::start(vec![
        stream_reply().hang(),
        stream_reply().send(text_response(&["ok"])),
    ])
    .await;
    let events: Vec<_> = model(&server, idle(150, 10_000))
        .await
        .stream(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
}
