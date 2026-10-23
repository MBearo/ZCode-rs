//! 读流：SSE 解码、取消与流空闲看门狗（docs/specs/rust-cli/model.md §5、stall-visibility.md §2.4）。
//!
//! 等下一块字节时同时等看门狗最近的醒来时刻：到了停顿时刻产出 `Stalled`；超过上限交回 [`Next::Idle`]，
//! 由 `transport` 决定静默重发还是报错。停顿之后收到计数的 SSE 事件时，先产出 `Resumed` 再交付事件。
//!
//! 分两层（修复线 F17）：[`Events`] 只经 [`Body`] 取响应体，看门狗的时刻都在这里定；[`Read`] 是
//! `Events<HttpResponse>` 加这次请求（取消令牌、静默重发）。单测把响应体换成按 tokio 时钟发块的脚本，
//! 在暂停的时钟上验证时刻（真实套接字加真实时钟的测试在负载下会多出停顿通知）。

use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelNotice, ModelStreamItem};
use rust_cli_net::sse::SseDecoder;
use rust_cli_net::{Bytes, HttpResponse, NetError};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::idle::{Limits, Wake, Watch};
use crate::protocol::{Failure, StreamDecoder};
use crate::transport::Connect;

/// 读流的下一步。
pub(crate) enum Next {
    Item(Result<ModelStreamItem, ModelError>),
    /// 超过了这个空闲上限：这次响应作废。
    Idle(Duration),
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
}

impl Read {
    pub(crate) fn new(
        response: HttpResponse,
        decoder: Box<dyn StreamDecoder>,
        limits: Limits,
        resumed: bool,
        connect: Box<Connect>,
    ) -> Self {
        Self {
            events: Events::new(response, decoder, limits, resumed),
            connect,
        }
    }

    pub(crate) fn delivered(&self) -> bool {
        self.events.delivered()
    }

    /// 丢掉响应（关闭连接），交回这次请求。
    pub(crate) fn into_connect(self: Box<Self>) -> Box<Connect> {
        self.connect
    }

    /// 下一项；流正常结束时为 `None`。
    pub(crate) async fn next(&mut self) -> Option<Next> {
        let cancel = self.connect.cancel().clone();
        self.events.next(&cancel).await
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
    /// 解码出错时，先交付它之前的事件，再交付错误。
    failed: Option<ModelError>,
    watch: Watch,
    /// 已经把这次响应的事件交给过调用方（看门狗中止后不能再静默重发）。
    delivered: bool,
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
            delivered: false,
        }
    }

    pub(crate) fn delivered(&self) -> bool {
        self.delivered
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
            if let Some(event) = self.pending.pop_front() {
                self.delivered = true;
                return item(Ok(ModelStreamItem::Event(event)));
            }
            if let Some(error) = self.failed.take() {
                return item(Err(error));
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
                    return item(Err(Failure::network(&error).error));
                }
                Some(Waited::Chunk(Ok(None))) => {
                    return item(Err(ModelError::Protocol {
                        message: "stream ended before message_stop".to_owned(),
                    }));
                }
                Some(Waited::Chunk(Ok(Some(chunk)))) => chunk,
            };
            let now = Instant::now();
            self.watch.bytes(now);
            let events = match self.sse.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    self.failed = Some(ModelError::Protocol {
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            for event in events {
                let ping = event.event.as_deref() == Some("ping");
                if self.watch.event(ping, now) {
                    self.notices.push_back(ModelNotice::Resumed);
                }
                if let Err(error) = self.decoder.decode(event, &mut self.pending) {
                    self.failed = Some(error);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "transport_read_tests.rs"]
mod tests;
