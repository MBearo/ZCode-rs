//! 读流的看门狗在暂停的 tokio 时钟上（stall-visibility.md §2.4、§7）：响应体是按虚拟时刻发块的脚本，
//! 结果只取决于脚本里的时刻，与机器负载和调度无关（修复线 F17）。

use std::collections::VecDeque;
use std::time::Duration;

use rust_cli_kernel_api::provider::WireProtocol;

use super::*;
use crate::errors::{Class, Vendor};
use crate::idle::{ByteIdle, StreamIdle};
use crate::protocol;

/// 按虚拟时刻（相对开始）发块的响应体；发完后停住，不结束。
struct Scripted {
    start: Instant,
    chunks: VecDeque<(Duration, Bytes)>,
}

impl Scripted {
    fn new(start: Instant, chunks: Vec<(u64, String)>) -> Self {
        let chunks = chunks
            .into_iter()
            .map(|(at, text)| (Duration::from_millis(at), Bytes::from(text)))
            .collect();
        Self { start, chunks }
    }
}

impl Body for Scripted {
    async fn chunk(&mut self) -> Result<Option<Bytes>, NetError> {
        let Some((at, _)) = self.chunks.front() else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(self.start + *at).await;
        // 到点之后才取走这一块：等待被看门狗的计时打断时，下次从同一块接着等。
        Ok(self.chunks.pop_front().map(|(_, bytes)| bytes))
    }
}

/// 事件级 `events` 毫秒、字节级 10 s、停顿通知 50 ms（与夹具测试相同的调小的上限）。
fn limits(events: u64) -> Limits {
    StreamIdle {
        events: Some(Duration::from_millis(events)),
        bytes: Some(ByteIdle::Fixed(Duration::from_secs(10))),
        stall_notice: Duration::from_millis(50),
    }
    .limits(Duration::from_secs(300))
}

fn sse(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn message_start() -> String {
    sse(
        "message_start",
        &r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","model":"<model>","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":25,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1}}}"#
            .replace("<model>", rust_cli_base::models::GLM_5_3),
    )
}

fn ping() -> String {
    sse("ping", r#"{"type":"ping"}"#)
}

fn events(start: Instant, chunks: Vec<(u64, String)>, limits: Limits) -> Events<Scripted> {
    let decoder = protocol::for_wire(WireProtocol::AnthropicMessages).decoder(Vendor::Generic);
    Events::new(Scripted::new(start, chunks), decoder, limits, false)
}

/// 一项的种类，事件写成 `event`。
fn shape(next: Option<Next>) -> String {
    match next {
        Some(Next::Item(Ok(ModelStreamItem::Event(_)))) => "event".to_owned(),
        Some(Next::Item(Ok(ModelStreamItem::Notice(ModelNotice::Stalled { retry_in_ms })))) => {
            format!("stalled({retry_in_ms})")
        }
        Some(Next::Item(Ok(ModelStreamItem::Notice(notice)))) => format!("{notice:?}"),
        Some(Next::Item(Err(error))) => format!("error({error:?})"),
        Some(Next::Idle(limit)) => format!("idle({limit:?})"),
        Some(Next::Early(verdict)) => format!("early({:?})", verdict.class),
        None => "end".to_owned(),
    }
}

/// 修 bug（F17）：这条原来是真实套接字加真实时钟的夹具测试（`tests/suite/stall.rs`），ping 间隔 20 ms、
/// 停顿时限 50 ms，负载下两块字节之间超过 50 ms 就多出 `stalled` / `resumed`，失败率约 5%–30%。
/// 改成暂停的时钟：时刻都是脚本给的，第 30 个 ping 之后的时刻关系是确定的。
#[tokio::test(start_paused = true)]
async fn only_thirty_pings_in_a_row_keep_the_stream_alive() {
    let start = Instant::now();
    // 0 ms 的 message_start，之后每 20 ms 一个 ping，共 60 个（最后一个在 1200 ms），然后停住。
    let mut chunks = vec![(0, message_start())];
    chunks.extend((1..=60).map(|index| (index * 20, ping())));
    let mut read = events(start, chunks, limits(150));
    let cancel = CancellationToken::new();
    // `message_start` 还没越过交出边界，扣着（修复线 F113，provider-errors.md §3.2）。
    assert_eq!(shape(read.next(&cancel).await), "idle(150ms)");
    // 第 30 个 ping（600 ms）之后事件级上限正好到点。ping 都算数的话要到第 60 个之后：先在 1250 ms 发停顿通知，
    // 1350 ms 才到点。
    assert_eq!(start.elapsed(), Duration::from_millis(750));
    assert!(read.received && !read.crossed);
}

/// 修 bug（F17）：原来是夹具测试（`tests/suite/stall.rs`），头一块与其余字节隔 120 ms、停顿时限 50 ms，`retry_in_ms`
/// 只能断言一个范围；负载下读流被拖过 120 ms 时，停顿计时与字节同时就绪、`select!` 随机选到字节，没有停顿通知
///（负载下 200 次里 1 次）。暂停的时钟上时刻是脚本给的：50 ms 停顿、离字节级上限正好 950 ms。
#[tokio::test(start_paused = true)]
async fn events_after_a_stall_resume_the_stream() {
    let start = Instant::now();
    let tail = [
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"slow"}}"#,
        ),
        sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":9}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();
    let limits = StreamIdle {
        events: Some(Duration::from_secs(10)),
        bytes: Some(ByteIdle::Fixed(Duration::from_millis(1_000))),
        stall_notice: Duration::from_millis(50),
    }
    .limits(Duration::from_secs(300));
    let mut read = events(start, vec![(0, message_start()), (120, tail)], limits);
    let cancel = CancellationToken::new();
    let mut shapes = Vec::new();
    loop {
        let next = shape(read.next(&cancel).await);
        if next == "end" {
            break;
        }
        shapes.push(next);
    }
    // `message_start` 扣到第一个非空的正文增量才交出（F113）：停顿通知照常先到。
    assert_eq!(
        shapes,
        [
            "stalled(950)",
            "Resumed",
            "event",
            "event",
            "event",
            "event",
            "event",
            "event"
        ]
    );
    assert_eq!(start.elapsed(), Duration::from_millis(120));
}

#[tokio::test(start_paused = true)]
async fn an_ordinary_event_counts_the_pings_again() {
    let start = Instant::now();
    // 30 个 ping（20–600 ms）之后一个普通事件（610 ms），再 30 个 ping（630–1210 ms）：都在计数里。
    let mut chunks = vec![(0, message_start())];
    chunks.extend((1..=30).map(|index| (index * 20, ping())));
    chunks.push((
        610,
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
    ));
    chunks.extend((1..=30).map(|index| (610 + index * 20, ping())));
    let mut read = events(start, chunks, limits(150));
    let cancel = CancellationToken::new();
    // 两个事件都没越过交出边界，扣着（F113）。
    // 最后一个 ping 在 1210 ms：停顿通知在 1260 ms（离事件级上限还有 100 ms），1360 ms 到点。
    assert_eq!(shape(read.next(&cancel).await), "stalled(100)");
    assert_eq!(shape(read.next(&cancel).await), "idle(150ms)");
    assert_eq!(start.elapsed(), Duration::from_millis(1360));
}

/// 读完就结束的响应体（截断的流）。
struct Finite(VecDeque<Bytes>);

impl Body for Finite {
    async fn chunk(&mut self) -> Result<Option<Bytes>, NetError> {
        Ok(self.0.pop_front())
    }
}

fn finite(chunks: Vec<String>) -> Events<Finite> {
    let decoder = protocol::for_wire(WireProtocol::AnthropicMessages).decoder(Vendor::Generic);
    let body = Finite(chunks.into_iter().map(Bytes::from).collect());
    Events::new(body, decoder, limits(10_000), false)
}

fn text_start() -> String {
    sse(
        "content_block_start",
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    )
}

fn text_delta(text: &str) -> String {
    sse(
        "content_block_delta",
        &format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}"#
        ),
    )
}

fn overloaded() -> String {
    sse(
        "error",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    )
}

/// 读到结束前的全部项。
async fn drain<B: Body>(read: &mut Events<B>) -> Vec<String> {
    let cancel = CancellationToken::new();
    let mut shapes = Vec::new();
    loop {
        let next = shape(read.next(&cancel).await);
        let last = next == "end" || next.starts_with("early") || next.starts_with("error");
        shapes.push(next);
        if last {
            return shapes;
        }
    }
}

/// 交出边界（provider-errors.md §3.2，修复线 F113）：`message_start`、块的开始、空增量与 ping 扣着，第一个非空的正文增量
/// 到达时按原序一起交出。
#[tokio::test(start_paused = true)]
async fn events_before_the_boundary_wait_for_the_first_text() {
    let start = Instant::now();
    let chunks = vec![
        (0, message_start()),
        (10, text_start()),
        (20, text_delta("")),
        (30, ping()),
        (40, text_delta("Hi")),
    ];
    let mut read = events(start, chunks, limits(10_000));
    let cancel = CancellationToken::new();
    assert_eq!(shape(read.next(&cancel).await), "event");
    // 第一项在非空增量到达（40 ms）时才交出。
    assert_eq!(start.elapsed(), Duration::from_millis(40));
    assert!(read.crossed);
    for _ in 0..3 {
        assert_eq!(shape(read.next(&cancel).await), "event");
    }
}

/// 边界之前失败：扣住的事件不交出，交回 `Early` 由 transport 决定重发（以前先交出 `message_start` 再交错误）。
#[tokio::test]
async fn a_failure_before_the_boundary_is_early() {
    let mut read = finite(vec![message_start(), text_start(), overloaded()]);
    assert_eq!(drain(&mut read).await, ["early(Overloaded)"]);
    let mut cut = finite(vec![message_start(), text_start()]);
    assert_eq!(drain(&mut cut).await, ["early(Truncated)"]);
    let mut invalid = finite(vec![
        message_start(),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":7,"delta":{"type":"text_delta","text":"x"}}"#,
        ),
    ]);
    assert_eq!(
        drain(&mut invalid).await,
        [format!("early({:?})", Class::Malformed)]
    );
}

/// 越过边界之后的失败照旧：先交出之前的事件，再把错误交给调用方（主循环部分收尾）。
#[tokio::test]
async fn a_failure_after_the_boundary_goes_to_the_caller() {
    let mut read = finite(vec![
        message_start(),
        text_start(),
        text_delta("partial"),
        overloaded(),
    ]);
    let shapes = drain(&mut read).await;
    assert_eq!(
        shapes,
        [
            "event",
            "event",
            "event",
            r#"error(Overloaded { message: "Overloaded" })"#
        ]
    );
    let mut cut = finite(vec![message_start(), text_start(), text_delta("partial")]);
    let shapes = drain(&mut cut).await;
    assert_eq!(
        shapes.last().map(String::as_str),
        Some(r#"error(Protocol { message: "stream ended before message_stop" })"#)
    );
}
