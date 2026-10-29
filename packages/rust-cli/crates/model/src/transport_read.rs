//! 读流：SSE 解码、取消与流空闲看门狗（docs/specs/rust-cli/model.md §5、stall-visibility.md §2.4）。
//!
//! 等下一块字节时同时等看门狗最近的醒来时刻：到了停顿时刻产出 `Stalled`；超过上限交回 [`Next::Idle`]，
//! 由 `transport` 决定静默重发还是报错。停顿之后收到计数的 SSE 事件时，先产出 `Resumed` 再交付事件。
//!
//! 分两层（修复线 F17）：[`Events`] 只经 [`Body`] 取响应体，看门狗的时刻都在这里定；[`Read`] 是
//! `Events<HttpResponse>` 加这次请求（取消令牌、静默重发）。单测把响应体换成按 tokio 时钟发块的脚本，
//! 在暂停的时钟上验证时刻（真实套接字加真实时钟的测试在负载下会多出停顿通知）。
//!
//! 交出边界（docs/specs/rust-cli/provider-errors.md §3.2，修复线 F113）：第一个非空的正文或思考增量、第一个块结束、
//! `MessageDelta`、`MessageStop` 越过边界，或解码器收到了终止事件。之前的事件（`MessageStart`、块的开始、工具参数、空增量）
//! 扣在这里，越过时按原序一起交出。边界之前失败时返回 [`Next::Early`]，扣住的事件随这次响应丢掉，由 transport 决定重发。

use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use rust_cli_kernel_api::model::{
    BlockDelta, ModelError, ModelEvent, ModelNotice, ModelStreamItem,
};
use rust_cli_net::sse::SseDecoder;
use rust_cli_net::{Bytes, HttpResponse, NetError};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::errors::{self, Verdict};
use crate::idle::{Limits, Wake, Watch};
use crate::model_io_tape::{End, Tape, end};
use crate::protocol::StreamDecoder;
use crate::transport::Connect;

/// 读流的下一步。
pub(crate) enum Next {
    Item(Result<ModelStreamItem, ModelError>),
    /// 超过了这个空闲上限：这次响应作废。
    Idle(Duration),
    /// 越过交出边界之前就失败了（P1）：扣住的事件不交出，这次响应作废，由 transport 按处置表决定。
    Early(Verdict),
}

/// 这个事件越过交出边界（provider-errors.md §3.2）。
fn crosses(event: &ModelEvent) -> bool {
    match event {
        ModelEvent::BlockDelta {
            delta: BlockDelta::Text(text) | BlockDelta::Thinking(text),
            ..
        } => !text.is_empty(),
        ModelEvent::BlockStop { .. }
        | ModelEvent::MessageDelta { .. }
        | ModelEvent::MessageStop => true,
        ModelEvent::MessageStart { .. }
        | ModelEvent::BlockStart { .. }
        | ModelEvent::BlockDelta { .. } => false,
    }
}

/// 等一块字节的结果。
enum Waited {
    Chunk(Result<Option<Bytes>, NetError>),
    Timer(Wake),
}

/// 响应体：逐块读取，读完时为 `None`。生产里是 [`HttpResponse`]。
pub(crate) trait Body: Send {
    fn chunk(&mut self) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send;
}

impl Body for HttpResponse {
    fn chunk(&mut self) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send {
        HttpResponse::chunk(self)
    }
}

/// 一次请求的读流。
pub(crate) struct Read {
    events: Events<HttpResponse>,
    /// 这次请求（取消令牌；看门狗静默重发时回到发送）。
    connect: Box<Connect>,
    /// 这次发送的模型轨迹（model-io.md §6）：流结束、出错或被取消时交出记录；丢弃时由它自己按丢弃记。
    tape: Option<Tape>,
}

impl Read {
    pub(crate) fn new(
        response: HttpResponse,
        decoder: Box<dyn StreamDecoder>,
        limits: Limits,
        resumed: bool,
        connect: Box<Connect>,
        tape: Option<Tape>,
    ) -> Self {
        Self {
            events: Events::new(response, decoder, limits, resumed),
            connect,
            tape,
        }
    }

    /// （解码出过事件，越过了交出边界）：看门狗按它决定静默重发、G 还是报错。
    pub(crate) fn progress(&self) -> (bool, bool) {
        (self.events.received, self.events.crossed)
    }

    /// 丢掉响应（关闭连接），交回这次请求与还没结束的录音（看门狗的结局由调用方按重发与否记）。
    pub(crate) fn into_parts(self: Box<Self>) -> (Box<Connect>, Option<Tape>) {
        let Self { connect, tape, .. } = *self;
        (connect, tape)
    }

    /// 下一项；流正常结束时为 `None`。流结束或交给调用方的出错时结束这次发送的录音（交出边界之前的失败由 transport
    /// 按重发与否记）。
    pub(crate) async fn next(&mut self) -> Option<Next> {
        let cancel = self.connect.cancel().clone();
        let next = self.events.next(&cancel).await;
        match &next {
            None => end(self.tape.take(), End::Completed),
            Some(Next::Item(Err(error))) => end(self.tape.take(), End::of_error(error)),
            Some(_) => {}
        }
        next
    }
}

/// 一次响应的事件：SSE 解码、流空闲看门狗与通知。
pub(crate) struct Events<B> {
    response: B,
    sse: SseDecoder,
    decoder: Box<dyn StreamDecoder>,
    /// 已解码、还没交给调用方的事件。一块字节可能含多个事件。
    pending: VecDeque<ModelEvent>,
    /// 先于事件交付的通知（`Resumed`）。
    notices: VecDeque<ModelNotice>,
    /// 解码出错时，越过了交出边界就先交付它之前的事件，再交付错误；没越过时这次响应作废。
    failed: Option<Verdict>,
    watch: Watch,
    /// 解码出过事件（看门狗的静默重发只在一个事件都没收到时做，stall-visibility.md §2.3）。
    received: bool,
    /// 越过了交出边界：`pending` 里的事件可以交出（provider-errors.md §3.2，修复线 F113）。
    crossed: bool,
}

fn item(result: Result<ModelStreamItem, ModelError>) -> Option<Next> {
    Some(Next::Item(result))
}

impl<B: Body> Events<B> {
    pub(crate) fn new(
        response: B,
        decoder: Box<dyn StreamDecoder>,
        limits: Limits,
        resumed: bool,
    ) -> Self {
        let notices = if resumed {
            VecDeque::from([ModelNotice::Resumed])
        } else {
            VecDeque::new()
        };
        Self {
            response,
            sse: SseDecoder::new(),
            decoder,
            pending: VecDeque::new(),
            notices,
            failed: None,
            watch: Watch::new(limits, Instant::now()),
            received: false,
            crossed: false,
        }
    }

    /// 一个失败：越过了交出边界交给调用方，没越过时作废这次响应（P1）。
    fn failure(&self, verdict: Verdict) -> Option<Next> {
        if self.crossed {
            item(Err(verdict.error))
        } else {
            Some(Next::Early(verdict))
        }
    }

    /// 下一项；流正常结束时为 `None`。等待随 `cancel` 结束。
    pub(crate) async fn next(&mut self, cancel: &CancellationToken) -> Option<Next> {
        loop {
            if let Some(notice) = self.notices.pop_front() {
                return item(Ok(ModelStreamItem::Notice(notice)));
            }
            if self.pending.is_empty() && self.failed.is_none() && self.decoder.finished() {
                return None;
            }
            if cancel.is_cancelled() {
                return item(Err(ModelError::Cancelled));
            }
            if self.crossed
                && let Some(event) = self.pending.pop_front()
            {
                return item(Ok(ModelStreamItem::Event(event)));
            }
            if let Some(verdict) = self.failed.take() {
                return self.failure(verdict);
            }
            let wake = self.watch.next();
            let chunk = self.response.chunk();
            let waited = cancel
                .run_until_cancelled(async move {
                    match wake {
                        None => Waited::Chunk(chunk.await),
                        Some((at, kind)) => tokio::select! {
                            chunk = chunk => Waited::Chunk(chunk),
                            () = tokio::time::sleep_until(at) => Waited::Timer(kind),
                        },
                    }
                })
                .await;
            let chunk = match waited {
                None => return item(Err(ModelError::Cancelled)),
                Some(Waited::Timer(Wake::Stall)) => {
                    let left = self.watch.stall(Instant::now());
                    let retry_in_ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX);
                    return item(Ok(ModelStreamItem::Notice(ModelNotice::Stalled {
                        retry_in_ms,
                    })));
                }
                Some(Waited::Timer(Wake::Idle(limit))) => return Some(Next::Idle(limit)),
                Some(Waited::Chunk(Err(error))) => {
                    return self.failure(errors::classify_transport(&error));
                }
                Some(Waited::Chunk(Ok(None))) => {
                    return self.failure(Verdict::truncated());
                }
                Some(Waited::Chunk(Ok(Some(chunk)))) => chunk,
            };
            let now = Instant::now();
            self.watch.bytes(now);
            let events = match self.sse.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    self.failed = Some(Verdict::malformed(error.to_string()));
                    continue;
                }
            };
            let before = self.pending.len();
            for event in events {
                let ping = event.event.as_deref() == Some("ping");
                if self.watch.event(ping, now) {
                    self.notices.push_back(ModelNotice::Resumed);
                }
                if let Err(verdict) = self.decoder.decode(event, &mut self.pending) {
                    self.failed = Some(verdict);
                    break;
                }
            }
            self.received |= self.pending.len() > before;
            // 流的终止事件之后没有别的内容：之前扣住的一起交出。
            self.crossed = self.crossed
                || self.decoder.finished()
                || self.pending.iter().skip(before).any(crosses);
        }
    }
}

#[cfg(test)]
#[path = "transport_read_tests.rs"]
mod tests;
