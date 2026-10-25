//! 一次调用与一个流在服务端的生命周期（remote-exec.md §18.5）：先记账再执行；回复与事件存进账本、发给这个客户端
//! 实例当前的连接；按 ack 删除；流的窗口（背压）与按 `seq` 补发。

use std::pin::pin;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use rust_cli_base::Task;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{
    Ack, EventHeader, ReplyHeader, ReplyResult, RequestHeader, StreamItem, WireError,
};
use rust_cli_exec_wire::frame::{Frame, Kind};
use rust_cli_exec_wire::handshake::{Capability, STREAM_WINDOW};
use rust_cli_exec_wire::request::Request;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::dispatch;
use crate::ledger::{Begun, CallKey, CallKind};
use crate::server::Inner;

/// 一个开着的流在服务端的运行状态（账本里是它的事件）。
pub(crate) struct StreamControl {
    acked: u64,
    resend_from: Option<u64>,
    ended_at: Option<u64>,
    notify: Arc<Notify>,
    task: Option<Task<()>>,
}

impl Drop for StreamControl {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// 请求内容的指纹：同一个调用 ID 再来时比较。
fn fingerprint(request: &Request, body: &Body) -> String {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(request).unwrap_or_default());
    hash.update(body.bytes());
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn encode_reply(call: u64, result: ReplyResult, body: &Body) -> Bytes {
    match Frame::encode(Kind::Reply, &ReplyHeader { call, result }, body) {
        Ok(payload) => payload,
        Err(error) => {
            let result = ReplyResult::Err(WireError::Io {
                message: error.to_string(),
            });
            Frame::encode(Kind::Reply, &ReplyHeader { call, result }, &Body::empty())
                .unwrap_or_default()
        }
    }
}

/// 不进账本的错误回复（解不开、围栏、能力位）。
pub(crate) async fn reply_error(inner: &Inner, instance: &str, call: u64, error: WireError) {
    let payload = encode_reply(call, ReplyResult::Err(error), &Body::empty());
    inner.send_to(instance, payload).await;
}

pub(crate) async fn request(inner: &Arc<Inner>, instance: &str, header: RequestHeader, body: Body) {
    let RequestHeader {
        call,
        request,
        resume_after,
    } = header;
    let op = request.op();
    if !inner.config.capabilities.contains(op.capability()) {
        let error = WireError::Unsupported {
            what: op.as_str().to_owned(),
        };
        return reply_error(inner, instance, call, error).await;
    }
    if let Err(error) = inner.config.fence.check(&request) {
        return reply_error(inner, instance, call, error).await;
    }
    let key = CallKey {
        instance: instance.to_owned(),
        call,
    };
    let kind = if op.is_stream() {
        CallKind::Stream
    } else if op.is_mutation() {
        CallKind::Mutation
    } else {
        CallKind::Read
    };
    let begun = match inner
        .ledger
        .begin(&key, &fingerprint(&request, &body), kind)
        .await
    {
        Ok(begun) => begun,
        Err(error) => {
            let error = WireError::Io {
                message: error.to_string(),
            };
            return reply_error(inner, instance, call, error).await;
        }
    };
    match begun {
        Begun::New if op.is_stream() => open_stream(inner, key, request),
        // 环境变量推送（F96，remote-exec.md §23.5）：在读循环里当场生效，之后读到的请求都看得到它。
        Begun::New if op == rust_cli_exec_wire::op::Op::EnvUpdate => {
            let result = match request {
                Request::EnvUpdate {
                    mode,
                    vars,
                    injected,
                } => match inner.env.update(instance, mode, vars, injected) {
                    Ok(()) => ReplyResult::Ok(rust_cli_exec_wire::reply::Reply::EnvUpdate),
                    Err(message) => ReplyResult::Err(WireError::BadRequest { message }),
                },
                _ => ReplyResult::Err(WireError::BadRequest {
                    message: "not an env.update".to_owned(),
                }),
            };
            let payload = encode_reply(call, result, &Body::empty());
            if let Err(error) = inner.ledger.finish(&key, payload.clone()).await {
                tracing::warn!(target: "exec_server", %error, "ledger finish failed");
            }
            inner.send_to(instance, payload).await;
        }
        Begun::New => {
            // 起进程的请求带上这个客户端实例推来的环境变量：在读到请求的这一刻取（F96，与 `env.update` 的先后一致）。
            let env = if matches!(
                op,
                rust_cli_exec_wire::op::Op::StartCommand | rust_cli_exec_wire::op::Op::McpConnect
            ) {
                inner.env.overrides(instance)
            } else {
                Default::default()
            };
            let inner_task = inner.clone();
            // 正在执行的调用数（常驻执行器的“空闲”判断，§19.5）：任务开始前加一，结束（含被中止）时减一。
            let running = crate::server::Running::start(inner);
            drop(inner.spawner.spawn("call", async move {
                let _running = running;
                let (result, body) = if request.op().capability() == Capability::Mcp {
                    // MCP 的管道要连接表与客户端实例（R5，§22.5）。
                    let result =
                        match crate::mcp::unary(&inner_task, &key.instance, request, env).await {
                            Ok(reply) => ReplyResult::Ok(reply),
                            Err(error) => ReplyResult::Err(error),
                        };
                    (result, Body::empty())
                } else {
                    dispatch::unary(&inner_task.machine, request, &body, env).await
                };
                let payload = encode_reply(key.call, result, &body);
                if let Err(error) = inner_task.ledger.finish(&key, payload.clone()).await {
                    tracing::warn!(target: "exec_server", %error, "ledger finish failed");
                }
                inner_task.send_to(&key.instance, payload).await;
            }));
        }
        // 补发：流从 `resume_after` 之后重发；没完成的调用等它完成（回复发到当前连接）。
        Begun::Running if op.is_stream() => resume_stream(inner, &key, resume_after.unwrap_or(0)),
        Begun::Running => {}
        Begun::Done(payload) => inner.send_to(instance, payload).await,
        Begun::Conflict => {
            let error = WireError::OperationConflict {
                operation: format!("call {call}"),
            };
            reply_error(inner, instance, call, error).await;
        }
        Begun::Interrupted => {
            let error = WireError::Interrupted {
                message: format!(
                    "{} did not finish before the executor restarted",
                    op.as_str()
                ),
            };
            reply_error(inner, instance, call, error).await;
        }
    }
}

fn open_stream(inner: &Arc<Inner>, key: CallKey, request: Request) {
    let notify = Arc::new(Notify::new());
    inner.streams().insert(
        key.clone(),
        StreamControl {
            acked: 0,
            resend_from: None,
            ended_at: None,
            notify: notify.clone(),
            task: None,
        },
    );
    let task = inner.spawner.spawn(
        "stream",
        produce(inner.clone(), key.clone(), request, notify),
    );
    match inner.streams().get_mut(&key) {
        Some(control) => control.task = Some(task),
        // 起任务之前流已经被关掉。
        None => task.abort(),
    }
}

fn resume_stream(inner: &Inner, key: &CallKey, after: u64) {
    if let Some(control) = inner.streams().get_mut(key) {
        control.resend_from = Some(after);
        control.notify.notify_one();
    }
}

/// 下一步要做什么（在锁里决定，锁外执行）。
enum Step {
    Resend(u64),
    Wait,
    Pull,
    Gone,
}

fn step(inner: &Inner, key: &CallKey, seq: u64) -> Step {
    let mut streams = inner.streams();
    let Some(control) = streams.get_mut(key) else {
        return Step::Gone;
    };
    if let Some(from) = control.resend_from.take() {
        return Step::Resend(from);
    }
    if control.ended_at.is_some() || seq.saturating_sub(control.acked) >= STREAM_WINDOW {
        return Step::Wait;
    }
    Step::Pull
}

/// 流的生产者：从端口取一项、编号、记账、发出；窗口满了就等 ack；收到补发请求就从账本重发。补发与新项都经这个任务，
/// 所以客户端收到的 `seq` 总是递增的（按 `seq` 去重不会丢项）。
async fn produce(inner: Arc<Inner>, key: CallKey, request: Request, notify: Arc<Notify>) {
    let mut items = if request.op().capability() == Capability::Mcp {
        crate::mcp::events(&inner, &key, request).await
    } else {
        dispatch::stream(&inner.machine, request)
    };
    let mut seq = 0u64;
    loop {
        let mut notified = pin!(notify.notified());
        notified.as_mut().enable();
        match step(&inner, &key, seq) {
            Step::Gone => return,
            Step::Resend(from) => {
                let events = inner
                    .ledger
                    .events_after(&key, from)
                    .await
                    .unwrap_or_default();
                for event in events {
                    inner.send_to(&key.instance, event).await;
                }
                continue;
            }
            Step::Wait => {
                notified.await;
                continue;
            }
            Step::Pull => {}
        }
        let next = tokio::select! {
            biased;
            () = &mut notified => continue,
            next = items.next() => next,
        };
        let (item, body) = next.unwrap_or((StreamItem::End, Body::empty()));
        seq += 1;
        let end = item == StreamItem::End;
        let header = EventHeader {
            call: key.call,
            seq,
            item,
        };
        let payload = match Frame::encode(Kind::Event, &header, &body) {
            Ok(payload) => payload,
            Err(error) => {
                let item = StreamItem::Error {
                    error: WireError::Io {
                        message: error.to_string(),
                    },
                };
                let header = EventHeader {
                    call: key.call,
                    seq,
                    item,
                };
                Frame::encode(Kind::Event, &header, &Body::empty()).unwrap_or_default()
            }
        };
        if let Err(error) = inner.ledger.append_event(&key, seq, payload.clone()).await {
            tracing::warn!(target: "exec_server", %error, "ledger append failed");
        }
        if end && let Some(control) = inner.streams().get_mut(&key) {
            control.ended_at = Some(seq);
        }
        inner.send_to(&key.instance, payload).await;
    }
}

/// 客户端的批量 ack：回复删掉；流丢掉 ack 过的事件、打开窗口，结束并 ack 到最后一项的流删掉。
pub(crate) async fn ack(inner: &Inner, instance: &str, ack: Ack) {
    for call in ack.replies {
        let key = CallKey {
            instance: instance.to_owned(),
            call,
        };
        let _ = inner.ledger.forget(&key).await;
    }
    for acked in ack.streams {
        let key = CallKey {
            instance: instance.to_owned(),
            call: acked.call,
        };
        let _ = inner.ledger.ack_events(&key, acked.seq).await;
        let finished = {
            let mut streams = inner.streams();
            let finished = match streams.get_mut(&key) {
                Some(control) => {
                    // 累计：乱序到达的 ack 取最大值。
                    control.acked = control.acked.max(acked.seq);
                    control.notify.notify_one();
                    control.ended_at.is_some_and(|end| control.acked >= end)
                }
                None => false,
            };
            if finished {
                streams.remove(&key);
            }
            finished
        };
        if finished {
            let _ = inner.ledger.forget(&key).await;
        }
    }
}

/// 客户端不再要这个流：停掉生产者，删掉账本里的记录。
pub(crate) async fn close(inner: &Inner, instance: &str, call: u64) {
    let key = CallKey {
        instance: instance.to_owned(),
        call,
    };
    let removed = inner.streams().remove(&key);
    drop(removed);
    let _ = inner.ledger.forget(&key).await;
}
