//! 一条连接（remote-exec.md §18.4、§18.5）：握手（主版本、鉴权、epoch）→ 读帧并派发、写任务发回复与事件 → 结束时
//! 只摘掉路由，不取消执行（断线不等于取消）。

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rust_cli_base::owned;
use rust_cli_exec_transport::{FrameSink, FrameSource, Link};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{Ack, Beat, Close, RequestHeader};
use rust_cli_exec_wire::frame::{Frame, Kind, Reassembler, Received, Splitter};
use rust_cli_exec_wire::handshake::{HEARTBEAT_MS, Hello, PROTOCOL, Refuse, RefuseCode, Welcome};
use tokio::sync::mpsc;

use crate::calls;
use crate::server::{Inner, Route, ServerError};

/// 等 `hello` 的时间。
const HANDSHAKE: Duration = Duration::from_secs(30);
/// 一条连接的发送队列。
const QUEUE: usize = 64;

async fn next_frame(
    source: &mut dyn FrameSource,
    reassembler: &mut Reassembler,
) -> Result<Option<Frame>, ServerError> {
    loop {
        let Some(bytes) = source.recv().await? else {
            return Ok(None);
        };
        match reassembler.push(bytes) {
            Ok(Received::Frame(frame)) => return Ok(Some(frame)),
            Ok(Received::Pending) => {}
            Ok(Received::Unknown(code)) => {
                tracing::debug!(target: "exec_server", code, "unknown frame kind ignored");
            }
            Err(error) => {
                tracing::warn!(target: "exec_server", %error, "bad frame; closing the connection");
                return Ok(None);
            }
        }
    }
}

async fn refuse(sink: &mut dyn FrameSink, code: RefuseCode, message: String) {
    let refuse = Refuse { code, message };
    if let Ok(payload) = Frame::encode(Kind::Refuse, &refuse, &Body::empty()) {
        let _ = sink.send(payload).await;
    }
    let _ = sink.close().await;
}

pub(crate) async fn serve(inner: Arc<Inner>, link: Box<dyn Link>) -> Result<(), ServerError> {
    let peer = link.peer();
    let (mut sink, mut source) = link.split();
    let mut reassembler = Reassembler::default();
    let first = match tokio::time::timeout(HANDSHAKE, next_frame(source.as_mut(), &mut reassembler))
        .await
    {
        Ok(first) => first?,
        Err(_) => return Ok(()),
    };
    let Some(first) = first.filter(|frame| frame.kind == Kind::Hello) else {
        return Ok(());
    };
    let Ok(hello) = first.header::<Hello>() else {
        refuse(
            sink.as_mut(),
            RefuseCode::Protocol,
            "hello is not valid".to_owned(),
        )
        .await;
        return Ok(());
    };
    if hello.protocol != PROTOCOL {
        let message = format!(
            "this executor speaks protocol {PROTOCOL}, the client {}",
            hello.protocol
        );
        refuse(sink.as_mut(), RefuseCode::Protocol, message).await;
        return Ok(());
    }
    let principal = match inner.auth.authenticate(&hello).await {
        Ok(principal) => principal,
        Err(message) => {
            refuse(sink.as_mut(), RefuseCode::Auth, message).await;
            return Ok(());
        }
    };
    let instance = principal.client_instance;
    let (data, data_rx) = mpsc::channel(QUEUE);
    let (control, control_rx) = mpsc::channel(QUEUE);
    // 同一实例更新的连接已经在了：这是旧连接，拒绝。新的替换旧的（旧连接的写任务随发送端被丢掉而结束）。
    let stale = {
        let mut routes = inner.routes();
        let stale = routes
            .get(&instance)
            .is_some_and(|route| route.epoch > hello.epoch);
        if !stale {
            routes.insert(
                instance.clone(),
                Route {
                    epoch: hello.epoch,
                    data,
                },
            );
        }
        stale
    };
    if stale {
        let message = "a newer connection of this client exists".to_owned();
        refuse(sink.as_mut(), RefuseCode::Stale, message).await;
        return Ok(());
    }
    // SSH 的代理加在 hello 里的这条 ssh 会话的环境（F96，remote-exec.md §23.5）：先于这条连接上的任何请求生效（请求在
    // welcome 之后才读）。不合规则时记 warn、忽略，连接照常。没报告 `env` 的执行器不认它。
    if let Some(session) = hello.session_env.clone()
        && inner
            .config
            .capabilities
            .contains(rust_cli_exec_wire::handshake::Capability::Env)
        && let Err(error) = inner.env.update(
            &instance,
            rust_cli_exec_wire::dto::env::EnvMode::Merge,
            session,
            Vec::new(),
        )
    {
        tracing::warn!(target: "exec_server", client = %instance, %error, "the session environment was ignored");
    }
    let config = &inner.config;
    let welcome = Welcome {
        protocol: PROTOCOL,
        server_version: config.server_version.clone(),
        server_instance: inner.instance.clone(),
        os: config.os.clone(),
        arch: config.arch.clone(),
        path_style: config.path_style,
        home: config.home.clone(),
        shell: config.shell.clone(),
        capabilities: config.capabilities.clone(),
        max_message: rust_cli_exec_wire::frame::MAX_MESSAGE,
        ledger: inner.ledger.identity(),
    };
    // welcome 直接写（写任务还没起）：之后的回复与事件都排在它后面。
    let welcome = Frame::encode(Kind::Welcome, &welcome, &Body::empty()).map_err(|error| {
        ServerError::Link(rust_cli_exec_transport::LinkError::Io(error.to_string()))
    })?;
    sink.send(welcome).await?;
    inner.connected(&instance);
    tracing::info!(target: "exec_server", %peer, client = %instance, epoch = hello.epoch, "client connected");
    let writer = owned(write_loop(sink, data_rx, control_rx, config.part_size));
    let heartbeat = Duration::from_millis(hello.heartbeat_ms.unwrap_or(HEARTBEAT_MS));
    let ended = read_loop(
        &inner,
        (&instance, hello.epoch),
        source,
        reassembler,
        control,
        heartbeat * 3,
    )
    .await;
    // 只摘掉自己的路由（更新的连接可能已经替换了它）；执行与流照常（remote-exec.md §18.5）。
    {
        let mut routes = inner.routes();
        if routes
            .get(&instance)
            .is_some_and(|route| route.epoch == hello.epoch)
        {
            routes.remove(&instance);
            drop(routes);
            inner.disconnected(&instance);
        }
    }
    drop(writer);
    tracing::info!(target: "exec_server", %peer, client = %instance, reason = %ended, "client disconnected");
    Ok(())
}

async fn read_loop(
    inner: &Arc<Inner>,
    (instance, epoch): (&str, u64),
    mut source: Box<dyn FrameSource>,
    mut reassembler: Reassembler,
    control: mpsc::Sender<Bytes>,
    liveness: Duration,
) -> String {
    loop {
        let frame =
            match tokio::time::timeout(liveness, next_frame(source.as_mut(), &mut reassembler))
                .await
            {
                Err(_) => return format!("no frame for {} ms", liveness.as_millis()),
                Ok(Err(error)) => return error.to_string(),
                Ok(Ok(None)) => return "closed".to_owned(),
                Ok(Ok(Some(frame))) => frame,
            };
        // 被同一实例更新的连接替换了：这条连接上的帧作废（remote-exec.md §18.4）。
        if inner
            .routes()
            .get(instance)
            .is_none_or(|route| route.epoch != epoch)
        {
            return "replaced by a newer connection".to_owned();
        }
        match frame.kind {
            Kind::Request => match RequestHeader::parse(&frame) {
                Ok(header) => calls::request(inner, instance, header, frame.body).await,
                Err(bad) => match bad.call {
                    Some(call) => calls::reply_error(inner, instance, call, bad.error).await,
                    None => return format!("bad request: {}", bad.error),
                },
            },
            Kind::Ack => match frame.header::<Ack>() {
                Ok(ack) => calls::ack(inner, instance, ack).await,
                Err(error) => return format!("bad ack: {error}"),
            },
            Kind::Close => match frame.header::<Close>() {
                Ok(close) => calls::close(inner, instance, close.call).await,
                Err(error) => return format!("bad close: {error}"),
            },
            Kind::Ping => {
                let nonce = frame
                    .header::<Beat>()
                    .map(|beat| beat.nonce)
                    .unwrap_or_default();
                if let Ok(pong) = Frame::encode(Kind::Pong, &Beat { nonce }, &Body::empty()) {
                    let _ = control.try_send(pong);
                }
            }
            Kind::Pong => {}
            other => return format!("unexpected {other:?} frame"),
        }
    }
}

async fn write_loop(
    mut sink: Box<dyn FrameSink>,
    mut data: mpsc::Receiver<Bytes>,
    mut control: mpsc::Receiver<Bytes>,
    part_size: usize,
) {
    let mut splitter = Splitter::new(part_size);
    loop {
        let frames = tokio::select! {
            biased;
            Some(frame) = control.recv() => vec![frame],
            Some(payload) = data.recv() => splitter.split(payload),
            else => break,
        };
        let count = frames.len();
        for (index, frame) in frames.into_iter().enumerate() {
            if sink.send(frame).await.is_err() {
                return;
            }
            // 大消息的两片之间先发控制帧（pong）。
            while index + 1 < count
                && let Ok(urgent) = control.try_recv()
            {
                if sink.send(urgent).await.is_err() {
                    return;
                }
            }
        }
    }
    let _ = sink.close().await;
}
