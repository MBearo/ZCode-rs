//! 连接的生命周期（docs/specs/rust-cli/remote-exec.md §18.4、§18.5）：
//!
//! ```text
//! 连上 → hello / welcome → 补发还没结束的调用 → 读、写、心跳三个任务 → 任一个结束（断线）
//!   └─ 还能重连：按退避等一会（0.5 s 起翻倍、有上限），再连上、握手（epoch + 1）、补发 …
//!   └─ 不能：还没结束的调用都以 Disconnected 结束
//! ```
//!
//! 每一步都发到连接的状态（`ConnectionState`，§19.8）：只有这个任务改它。
//!
//! 要发的请求只排调用 ID，写任务发的时候才按 `pending` 编码：已经结束的（例如执行器换了实例、被判为 `Interrupted`
//! 的）不会再发出去，同一次连接上也不重复发。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use rust_cli_base::owned;
use rust_cli_exec_transport::{Connector, FrameSink, FrameSource};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{
    Beat, Close, EventHeader, ReplyHeader, ReplyResult, RequestHeader, StreamItem,
};
use rust_cli_exec_wire::frame::{Frame, Kind, Reassembler, Received, Splitter};
use rust_cli_exec_wire::handshake::{Capabilities, Hello, PROTOCOL, Refuse, Welcome};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::state::{Control, Pending, Shared, Waiter, fail};
use crate::{Answer, ClientError, ConnectionState, Reconnect, StreamEvent};

/// 一条握手完成的连接。
pub(crate) struct Live {
    sink: Box<dyn FrameSink>,
    source: Box<dyn FrameSource>,
    reassembler: Reassembler,
    welcome: Welcome,
}

/// 要发的东西（跨连接保留：写任务结束时交回）。
pub(crate) struct Queues {
    pub requests: mpsc::Receiver<u64>,
    pub control: mpsc::Receiver<Control>,
}

fn liveness(shared: &Shared) -> Duration {
    shared.options.heartbeat * 3
}

async fn next_frame(
    source: &mut dyn FrameSource,
    reassembler: &mut Reassembler,
) -> Result<Frame, ClientError> {
    loop {
        let bytes = source
            .recv()
            .await
            .map_err(|error| ClientError::Connect(error.to_string()))?
            .ok_or_else(|| ClientError::Connect("the executor closed the connection".to_owned()))?;
        match reassembler.push(bytes)? {
            Received::Frame(frame) => return Ok(frame),
            Received::Pending | Received::Unknown(_) => {}
        }
    }
}

/// 连上并握手（第一次连接与重连都经这里）。
pub(crate) async fn connect_once(
    connector: &dyn Connector,
    shared: &Shared,
) -> Result<Live, ClientError> {
    let link = connector
        .connect()
        .await
        .map_err(|error| ClientError::Connect(error.to_string()))?;
    let (mut sink, mut source) = link.split();
    let epoch = {
        let mut state = shared.state();
        state.epoch += 1;
        state.epoch
    };
    let options = &shared.options;
    let hello = Hello {
        protocol: PROTOCOL,
        client_version: options.client_version.clone(),
        client_instance: options.client_instance.clone(),
        epoch,
        capabilities: Capabilities::ports(),
        heartbeat_ms: Some(u64::try_from(options.heartbeat.as_millis()).unwrap_or(u64::MAX)),
        credentials: options.credentials.clone(),
    };
    sink.send(Frame::encode(Kind::Hello, &hello, &Body::empty())?)
        .await
        .map_err(|error| ClientError::Connect(error.to_string()))?;
    let mut reassembler = Reassembler::default();
    let first = tokio::time::timeout(
        liveness(shared),
        next_frame(source.as_mut(), &mut reassembler),
    )
    .await
    .map_err(|_| ClientError::Connect("the executor did not answer the handshake".to_owned()))??;
    match first.kind {
        Kind::Welcome => {
            let welcome: Welcome = first.header()?;
            if welcome.protocol != PROTOCOL {
                return Err(ClientError::Protocol(format!(
                    "the executor speaks protocol {}, this client {PROTOCOL}",
                    welcome.protocol
                )));
            }
            // 握手一完成就记下能力位：调用方马上可以按它发请求（排进队列，连接跑起来之后发出）。
            shared.state().welcome = Some(welcome.clone());
            Ok(Live {
                sink,
                source,
                reassembler,
                welcome,
            })
        }
        Kind::Refuse => {
            let refuse: Refuse = first.header()?;
            Err(ClientError::Refused {
                code: refuse.code,
                message: refuse.message,
            })
        }
        other => Err(ClientError::Protocol(format!(
            "expected welcome, got {other:?}"
        ))),
    }
}

/// 管连接的任务：跑一条连接，断了就重连或结束。
pub(crate) async fn supervise(
    shared: Arc<Shared>,
    connector: Arc<dyn Connector>,
    mut live: Live,
    mut queues: Queues,
) {
    let mut previous: Option<Welcome> = None;
    loop {
        let welcome = live.welcome.clone();
        let (returned, reason) = run(&shared, live, queues, previous.as_ref()).await;
        queues = returned;
        previous = Some(welcome);
        if shared.shutdown.is_cancelled() {
            shared.state().fail_all(&ClientError::Closed);
            return;
        }
        shared.state().connected = false;
        shared
            .connection
            .send_replace(ConnectionState::Reconnecting {
                attempt: 0,
                reason: reason.clone(),
            });
        tracing::warn!(target: "exec_client", reason = %reason, "executor connection lost");
        match reconnect(&shared, connector.as_ref(), &reason).await {
            Ok(next) => live = next,
            Err(error) => {
                let error = match error {
                    ClientError::Closed => ClientError::Closed,
                    refused @ ClientError::Refused { .. } => refused,
                    other => ClientError::Disconnected(format!("{reason}; {other}")),
                };
                let mut state = shared.state();
                state.closed = Some(error.clone());
                state.fail_all(&error);
                drop(state);
                shared
                    .connection
                    .send_replace(ConnectionState::Closed { error });
                return;
            }
        }
    }
}

/// 第 `attempt` 次重连（从 1 起）之前等多久：`delay` 起每次翻倍，最多 `max_delay`（§19.8）。
pub fn backoff(policy: Reconnect, attempt: u32) -> std::time::Duration {
    let factor = 1u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    policy
        .delay
        .checked_mul(factor)
        .unwrap_or(policy.max_delay)
        .min(policy.max_delay.max(policy.delay))
}

async fn reconnect(
    shared: &Shared,
    connector: &dyn Connector,
    reason: &str,
) -> Result<Live, ClientError> {
    let policy = shared.options.reconnect;
    let mut last = ClientError::Connect("reconnecting is off".to_owned());
    for attempt in 1..=policy.attempts {
        tokio::select! {
            () = tokio::time::sleep(backoff(policy, attempt)) => {}
            () = shared.shutdown.cancelled() => return Err(ClientError::Closed),
        }
        shared
            .connection
            .send_replace(ConnectionState::Reconnecting {
                attempt,
                reason: reason.to_owned(),
            });
        match connect_once(connector, shared).await {
            Ok(live) => return Ok(live),
            // 被拒绝、协议不对：再连也一样。
            Err(error @ (ClientError::Refused { .. } | ClientError::Protocol(_))) => {
                return Err(error);
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// 跑一条连接直到它断开；交回队列与断开的原因。
async fn run(
    shared: &Arc<Shared>,
    live: Live,
    queues: Queues,
    previous: Option<&Welcome>,
) -> (Queues, String) {
    let Live {
        sink,
        source,
        reassembler,
        welcome,
    } = live;
    let resend = resume(shared, &welcome, previous);
    let stop = CancellationToken::new();
    let seen = Arc::new(Mutex::new(Instant::now()));
    let mut writer = owned(write_loop(shared.clone(), sink, queues, stop.clone()));
    let mut reader = owned(read_loop(shared.clone(), source, reassembler, seen.clone()));
    let mut heartbeat = owned(heartbeat_loop(shared.clone(), seen));
    for call in resend {
        if shared.requests.send(call).await.is_err() {
            break;
        }
    }
    let reason = tokio::select! {
        written = &mut writer => match written {
            Ok((queues, reason)) => return (queues, reason),
            Err(error) => format!("writer task ended: {error}"),
        },
        read = &mut reader => read.unwrap_or_else(|error| format!("reader task ended: {error}")),
        beat = &mut heartbeat => beat.unwrap_or_else(|error| format!("heartbeat task ended: {error}")),
        () = shared.shutdown.cancelled() => "client closed".to_owned(),
    };
    stop.cancel();
    match writer.await {
        Ok((queues, _)) => (queues, reason),
        // 写任务 panic 时队列丢了：重建一对（旧的发送端随之失效，调用方会得到 Closed）。
        Err(_) => {
            let (_, requests) = mpsc::channel(1);
            let (_, control) = mpsc::channel(1);
            (Queues { requests, control }, reason)
        }
    }
}

/// 握手完成：记下 welcome，决定补发哪些调用（remote-exec.md §18.5、§19.8）。执行器换了实例时，在旧实例上发出过的流
/// 以 `Interrupted` 结束（消费方按自己的游标重新订阅）；有副作用的调用看账本：新实例报告的账本与旧实例相同时照常补发，
/// 由账本决定结果（完成的取回、重启前没有终态的回 `interrupted`、没登记过的照常执行——先记账再执行，所以没登记过就是
/// 没开始过）；账本不同或没有时以 `Interrupted` 结束，不重跑。
fn resume(shared: &Shared, welcome: &Welcome, previous: Option<&Welcome>) -> Vec<u64> {
    let restarted =
        previous.is_some_and(|previous| previous.server_instance != welcome.server_instance);
    let same_ledger = previous
        .and_then(|previous| previous.ledger.as_ref())
        .is_some_and(|ledger| welcome.ledger.as_ref() == Some(ledger));
    let mut state = shared.state();
    state.welcome = Some(welcome.clone());
    state.connected = true;
    let epoch = state.epoch;
    if restarted {
        let lost: Vec<u64> = state
            .pending
            .iter()
            .filter(|(_, pending)| {
                let op = pending.request.op();
                pending.sent_epoch != 0 && (op.is_stream() || (op.is_mutation() && !same_ledger))
            })
            .map(|(call, _)| *call)
            .collect();
        for call in lost {
            if let Some(pending) = state.pending.remove(&call) {
                let op = pending.request.op().as_str().to_owned();
                fail(pending.waiter, ClientError::Interrupted { op });
            }
        }
    }
    let resend = state.pending.keys().copied().collect();
    drop(state);
    connected(shared, epoch, &welcome.server_instance);
    resend
}

/// 状态改成“连上”（与现在相同时不通知：第一次连接在 `ExecClient::connect` 里已经发过）。
pub(crate) fn connected(shared: &Shared, epoch: u64, server_instance: &str) {
    let next = ConnectionState::Connected {
        epoch,
        server_instance: server_instance.to_owned(),
    };
    shared.connection.send_if_modified(|state| {
        if *state == next {
            return false;
        }
        *state = next;
        true
    });
}

/// 编码一个要发的调用；已经结束或这次连接上发过的不发。
fn encode_call(shared: &Shared, call: u64) -> Option<Bytes> {
    let mut state = shared.state();
    let epoch = state.epoch;
    let pending = state.pending.get_mut(&call)?;
    if pending.sent_epoch == epoch {
        return None;
    }
    let resume_after = match pending.waiter {
        Waiter::Stream { received, .. } if pending.sent_epoch != 0 => Some(received),
        _ => None,
    };
    pending.sent_epoch = epoch;
    let header = RequestHeader {
        call,
        request: pending.request.clone(),
        resume_after,
    };
    match Frame::encode(Kind::Request, &header, &pending.body) {
        Ok(payload) => Some(payload),
        Err(error) => {
            if let Some(Pending { waiter, .. }) = state.pending.remove(&call) {
                fail(waiter, error.into());
            }
            None
        }
    }
}

fn encode_control(control: Control) -> Option<Bytes> {
    let encoded = match control {
        Control::Ping(nonce) => Frame::encode(Kind::Ping, &Beat { nonce }, &Body::empty()),
        Control::Pong(nonce) => Frame::encode(Kind::Pong, &Beat { nonce }, &Body::empty()),
        Control::Close(call) => Frame::encode(Kind::Close, &Close { call }, &Body::empty()),
    };
    encoded.ok()
}

fn encode_ack(shared: &Shared) -> Option<Bytes> {
    let ack = shared.state().take_ack()?;
    Frame::encode(Kind::Ack, &ack, &Body::empty()).ok()
}

/// 控制帧与 ack：大消息的两片之间先发它们。
fn urgent(shared: &Shared, queues: &mut Queues) -> Vec<Bytes> {
    let mut frames = Vec::new();
    while let Ok(control) = queues.control.try_recv() {
        frames.extend(encode_control(control));
    }
    frames.extend(encode_ack(shared));
    frames
}

async fn send(sink: &mut dyn FrameSink, frame: Bytes, stop: &CancellationToken) -> Option<String> {
    tokio::select! {
        sent = sink.send(frame) => sent.err().map(|error| error.to_string()),
        () = stop.cancelled() => Some("connection stopped".to_owned()),
    }
}

async fn write_loop(
    shared: Arc<Shared>,
    mut sink: Box<dyn FrameSink>,
    mut queues: Queues,
    stop: CancellationToken,
) -> (Queues, String) {
    let mut splitter = Splitter::new(shared.options.part_size);
    loop {
        let frames: Vec<Bytes> = tokio::select! {
            biased;
            () = stop.cancelled() => return (queues, "connection stopped".to_owned()),
            () = shared.shutdown.cancelled() => {
                let _ = sink.close().await;
                return (queues, "client closed".to_owned());
            }
            Some(control) = queues.control.recv() => encode_control(control).into_iter().collect(),
            () = shared.ack.notified() => encode_ack(&shared).into_iter().collect(),
            Some(call) = queues.requests.recv() => {
                encode_call(&shared, call).map(|payload| splitter.split(payload)).unwrap_or_default()
            }
        };
        let count = frames.len();
        for (index, frame) in frames.into_iter().enumerate() {
            if let Some(reason) = send(sink.as_mut(), frame, &stop).await {
                return (queues, reason);
            }
            if index + 1 < count {
                for frame in urgent(&shared, &mut queues) {
                    if let Some(reason) = send(sink.as_mut(), frame, &stop).await {
                        return (queues, reason);
                    }
                }
            }
        }
    }
}

async fn read_loop(
    shared: Arc<Shared>,
    mut source: Box<dyn FrameSource>,
    mut reassembler: Reassembler,
    seen: Arc<Mutex<Instant>>,
) -> String {
    loop {
        let bytes = match source.recv().await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return "the executor closed the connection".to_owned(),
            Err(error) => return error.to_string(),
        };
        if let Ok(mut seen) = seen.lock() {
            *seen = Instant::now();
        }
        match reassembler.push(bytes) {
            Ok(Received::Frame(frame)) => {
                if let Err(reason) = dispatch(&shared, frame) {
                    return reason;
                }
            }
            Ok(Received::Pending) => {}
            Ok(Received::Unknown(code)) => {
                tracing::debug!(target: "exec_client", code, "unknown frame kind ignored");
            }
            Err(error) => return format!("bad frame from the executor: {error}"),
        }
    }
}

/// 处理一帧；违反协议时返回断开的原因。
fn dispatch(shared: &Shared, frame: Frame) -> Result<(), String> {
    match frame.kind {
        Kind::Reply => {
            let header: ReplyHeader = frame.header().map_err(|error| error.to_string())?;
            let mut state = shared.state();
            state.ack_reply(header.call);
            let waiter = state
                .pending
                .remove(&header.call)
                .map(|pending| pending.waiter);
            drop(state);
            shared.ack.notify_one();
            match waiter {
                Some(Waiter::Unary(tx)) => {
                    let _ = tx.send(check_reply(header.result, frame.body));
                }
                Some(stream @ Waiter::Stream { .. }) => fail(
                    stream,
                    ClientError::Protocol("a stream got a reply".to_owned()),
                ),
                // 补发后的重复回复。
                None => {}
            }
            Ok(())
        }
        Kind::Event => {
            let header: EventHeader = frame.header().map_err(|error| error.to_string())?;
            deliver(shared, header, frame.body);
            Ok(())
        }
        Kind::Ping => {
            let nonce = frame
                .header::<Beat>()
                .map(|beat| beat.nonce)
                .unwrap_or_default();
            let _ = shared.control.try_send(Control::Pong(nonce));
            Ok(())
        }
        // 远端执行 R4（remote-exec.md §21.2）：最近一次 ping 的 pong 回来时记下往返时间；对不上的（旧连接、重复）不算。
        Kind::Pong => {
            let nonce = frame.header::<Beat>().map(|beat| beat.nonce).ok();
            let sent = {
                let mut state = shared.state();
                match state.beat {
                    Some((sent, at)) if Some(sent) == nonce => {
                        state.beat = None;
                        Some(at)
                    }
                    _ => None,
                }
            };
            if let Some(at) = sent {
                shared.latency.send_replace(Some(at.elapsed()));
            }
            Ok(())
        }
        other => Err(format!("unexpected {other:?} frame from the executor")),
    }
}

fn check_reply(result: ReplyResult, body: Body) -> Result<Answer, ClientError> {
    Ok(Answer { result, body })
}

/// 流的一项：按 `seq` 去重后交给消费方；`end` 不交，收到就结束这个流并 ack。
fn deliver(shared: &Shared, header: EventHeader, body: Body) {
    let mut state = shared.state();
    let Some(pending) = state.pending.get_mut(&header.call) else {
        return;
    };
    let Waiter::Stream { tx, received } = &mut pending.waiter else {
        return;
    };
    if header.seq <= *received {
        return;
    }
    *received = header.seq;
    if header.item == StreamItem::End {
        state.pending.remove(&header.call);
        state.ack_stream(header.call, header.seq);
        drop(state);
        shared.ack.notify_one();
        return;
    }
    let event = StreamEvent {
        item: header.item,
        body,
    };
    match tx.try_send(Ok((header.seq, event))) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Closed(_)) => {
            // 消费方已经不要了（它丢掉流时也会发 close；这里是兜底，重复的 close 无害）。
            state.pending.remove(&header.call);
            let _ = shared.control.try_send(Control::Close(header.call));
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            if let Some(pending) = state.pending.remove(&header.call) {
                fail(
                    pending.waiter,
                    ClientError::Protocol("the executor exceeded the stream window".to_owned()),
                );
            }
        }
    }
}

/// 心跳：连上就先 ping 一次（远端执行 R4：马上量出延迟，remote-exec.md §21.2），之后每个间隔一次；三个间隔没收到任何
/// 帧就当连接断了。
async fn heartbeat_loop(shared: Arc<Shared>, seen: Arc<Mutex<Instant>>) -> String {
    let interval = shared.options.heartbeat;
    loop {
        ping(&shared);
        tokio::time::sleep(interval).await;
        let quiet = seen.lock().map(|seen| seen.elapsed()).unwrap_or_default();
        if quiet > liveness(&shared) {
            return format!("no frame from the executor for {} ms", quiet.as_millis());
        }
    }
}

/// 发一个 ping，记下它的 nonce 与时刻（pong 回来时算往返）。
fn ping(shared: &Shared) {
    let nonce = {
        let mut state = shared.state();
        state.beats += 1;
        let nonce = state.beats;
        state.beat = Some((nonce, Instant::now()));
        nonce
    };
    let _ = shared.control.try_send(Control::Ping(nonce));
}
