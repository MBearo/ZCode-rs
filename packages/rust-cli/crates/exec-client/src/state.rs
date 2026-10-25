//! 客户端的共享状态：还没结束的调用、待发的 ack、当前的握手结果。锁只在取放时短暂持有，不跨 await。

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{Ack, StreamAck};
use rust_cli_exec_wire::handshake::Welcome;
use rust_cli_exec_wire::request::Request;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::{Answer, ClientError, ClientOptions, ConnectionState, StreamEvent};

/// 一个流向消费方交的东西：`seq` 与这一项。
pub(crate) type Delivered = Result<(u64, StreamEvent), ClientError>;

pub(crate) enum Waiter {
    Unary(oneshot::Sender<Result<Answer, ClientError>>),
    Stream {
        tx: mpsc::Sender<Delivered>,
        /// 已经收到（交给消费方）的最后一项的 `seq`：去重与补发的起点。
        received: u64,
    },
}

/// 一次还没结束的调用：补发时按它重新编码。
pub(crate) struct Pending {
    pub request: Request,
    pub body: Body,
    pub waiter: Waiter,
    /// 在哪一次连接上发过（同一次连接不重复发）。
    pub sent_epoch: u64,
}

/// 控制帧：在大消息的两片之间也能插进去。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Ping(u64),
    Pong(u64),
    Close(u64),
}

#[derive(Default)]
pub(crate) struct State {
    pub next_call: u64,
    pub pending: BTreeMap<u64, Pending>,
    replies_to_ack: Vec<u64>,
    streams_to_ack: BTreeMap<u64, u64>,
    pub welcome: Option<Welcome>,
    /// 当前（或下一次）连接的序号：握手的 `epoch`。
    pub epoch: u64,
    /// 连接已就绪（握手完成、还没断）。
    pub connected: bool,
    /// 不再工作的原因（关闭或断线后不再重连）。
    pub closed: Option<ClientError>,
    /// 发过的心跳个数（nonce 的来源，跨连接递增）。
    pub beats: u64,
    /// 最近一次 ping 的 nonce 与发出的时刻：同一个 nonce 的 pong 回来时算往返（远端执行 R4，remote-exec.md §21.2）。
    pub beat: Option<(u64, tokio::time::Instant)>,
}

impl State {
    pub fn ack_reply(&mut self, call: u64) {
        self.replies_to_ack.push(call);
    }

    /// 消费方取走了 `seq`：累计 ack，乱序取最大值。
    pub fn ack_stream(&mut self, call: u64, seq: u64) {
        let acked = self.streams_to_ack.entry(call).or_default();
        *acked = (*acked).max(seq);
    }

    /// 取走待发的 ack；没有时 None。
    pub fn take_ack(&mut self) -> Option<Ack> {
        if self.replies_to_ack.is_empty() && self.streams_to_ack.is_empty() {
            return None;
        }
        Some(Ack {
            replies: std::mem::take(&mut self.replies_to_ack),
            streams: std::mem::take(&mut self.streams_to_ack)
                .into_iter()
                .map(|(call, seq)| StreamAck { call, seq })
                .collect(),
        })
    }

    /// 结束全部还没结束的调用。还没在任何一次连接上发出去的以 `NotSent` 结束（执行器没收到，没有执行；F96，
    /// remote-exec.md §23.6），发出过的以 `error` 结束（结果不明）。
    pub fn fail_all(&mut self, error: &ClientError) {
        for (_, pending) in std::mem::take(&mut self.pending) {
            let error = if pending.sent_epoch == 0 {
                not_sent(error)
            } else {
                error.clone()
            };
            fail(pending.waiter, error);
        }
    }
}

/// 没发出去的调用的错误（已经是 `NotSent` 时不再包一层）。
pub(crate) fn not_sent(error: &ClientError) -> ClientError {
    match error {
        ClientError::NotSent(_) => error.clone(),
        other => ClientError::NotSent(Box::new(other.clone())),
    }
}

/// 以错误结束一个调用（流：交一个错误，之后结束）。
pub(crate) fn fail(waiter: Waiter, error: ClientError) {
    match waiter {
        Waiter::Unary(tx) => {
            let _ = tx.send(Err(error));
        }
        Waiter::Stream { tx, .. } => {
            let _ = tx.try_send(Err(error));
        }
    }
}

pub(crate) struct Shared {
    state: Mutex<State>,
    pub options: ClientOptions,
    /// 要发的请求（调用 ID）：写任务发的时候才按 `pending` 编码，已经结束的跳过。
    pub requests: mpsc::Sender<u64>,
    pub control: mpsc::Sender<Control>,
    /// 有新的 ack 要发。
    pub ack: Notify,
    /// 客户端关闭。
    pub shutdown: CancellationToken,
    /// 连接的状态（§19.8）；只由管连接的任务（与 `close`）改。
    pub connection: watch::Sender<ConnectionState>,
    /// 最近一次心跳的往返时间（R4，§21.2）；还没量到时 None。只由读任务改。
    pub latency: watch::Sender<Option<std::time::Duration>>,
    /// 叫醒在等退避的重连（F98，remote-exec.md §25.4.1：用户的“立即重试”）。
    pub retry: Notify,
}

impl Shared {
    pub fn new(
        options: ClientOptions,
        requests: mpsc::Sender<u64>,
        control: mpsc::Sender<Control>,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                next_call: 1,
                epoch: 0,
                ..State::default()
            }),
            options,
            requests,
            control,
            ack: Notify::new(),
            shutdown: CancellationToken::new(),
            connection: watch::channel(ConnectionState::Closed {
                error: ClientError::Connect("not connected yet".to_owned()),
            })
            .0,
            latency: watch::channel(None).0,
            retry: Notify::new(),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        // 锁里的代码不 panic；真的中毒时照常取出（状态仍然一致：每次修改都是一步完成的）。
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
