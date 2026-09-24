//! App Server event loop: JSON-RPC routing, delivery and host bridging.
//!
//! Ordering rules:
//! - The loop never awaits the runtime. Requests are queued and sent when the
//!   runtime can accept them, while runtime output is always drained, so the
//!   two bounded channels cannot deadlock.
//! - A runtime reply is written before the events that follow it in the same
//!   runtime batch, matching the Node runtime (response line, then frames).
use super::{Sink, delivery::Delivery};
use crate::contract::{ClientMsg, Input, Method, RuntimeError, ServerMsg};
use crate::domain::protocol::{Request, RequestId};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;

/// Queued runtime requests above which stdin is not read (backpressure to the reader thread).
const MAX_QUEUED_REQUESTS: usize = 64;
const RUNTIME_CHANNEL: usize = 64;

pub(super) enum Pending {
    Rpc(Option<RequestId>),
    Subscribe {
        id: Option<RequestId>,
        topic: String,
        connection: String,
        profile: crate::domain::delivery::Profile,
    },
    Resync {
        id: Option<RequestId>,
        subscription: String,
    },
    Recover {
        subscription: String,
    },
}

pub(super) struct Server {
    pub(super) sink: Sink,
    pub(super) delivery: Delivery,
    pub(super) to_runtime: VecDeque<ClientMsg>,
    pub(super) calls: HashMap<u64, Pending>,
    pub(super) next_token: u64,
    pub(super) input_open: bool,
    pub(super) congested: bool,
}

/// Run the App Server over `runtime`, reading parsed wire input and writing to `sink`.
/// Returns the runtime's result after it stops and its output is drained.
pub async fn serve<F, Fut>(runtime: F, mut input: mpsc::Receiver<Input>, sink: Sink) -> Result<()>
where
    F: FnOnce(mpsc::Receiver<ClientMsg>, mpsc::Sender<Vec<ServerMsg>>) -> Fut,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let (client_tx, client_rx) = mpsc::channel(RUNTIME_CHANNEL);
    let (server_tx, mut server_rx) = mpsc::channel(RUNTIME_CHANNEL);
    let running = tokio::spawn(runtime(client_rx, server_tx));
    let mut server = Server {
        sink,
        delivery: Delivery::default(),
        to_runtime: VecDeque::new(),
        calls: HashMap::new(),
        next_token: 0,
        input_open: true,
        congested: false,
    };
    let mut drain = tokio::time::interval(std::time::Duration::from_millis(100));
    drain.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // 订阅者缓冲的刷新窗口（spec rust-m8-delivery §5）；拥塞期间由 drain 恢复。
        let due = server.delivery.next_due().filter(|_| !server.congested);
        let flush_at = due.map_or_else(tokio::time::Instant::now, tokio::time::Instant::from_std);
        tokio::select! {
            biased;
            batch = server_rx.recv() => match batch {
                Some(batch) => server.on_runtime(batch).await?,
                None => {
                    server.flush_final().await?;
                    break;
                }
            },
            _ = tokio::time::sleep_until(flush_at), if due.is_some() => server.flush().await?,
            permit = client_tx.reserve(), if !server.to_runtime.is_empty() => match permit {
                Ok(permit) => permit.send(server.to_runtime.pop_front().expect("queue is not empty")),
                // runtime 已停止接收；剩余请求没有 owner，等待其输出关闭后退出。
                Err(_) => server.to_runtime.clear(),
            },
            message = input.recv(), if server.input_open && server.to_runtime.len() < MAX_QUEUED_REQUESTS => match message {
                Some(message) => server.on_input(message).await?,
                None => server.end_input(),
            },
            _ = drain.tick(), if server.congested => server.relieve().await?,
        }
    }
    running.await?
}

pub(super) fn response(id: RequestId, result: Result<Value, RuntimeError>) -> String {
    match result {
        Ok(value) => json!({"id":id,"result":value}),
        Err(error) => json!({"id":id,"error":error.to_json()}),
    }
    .to_string()
}

pub(super) fn required<'a>(p: &'a Value, key: &str) -> Result<&'a str, RuntimeError> {
    p[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RuntimeError::InvalidParams(format!("{key}: required")))
}

pub(super) fn not_owned() -> RuntimeError {
    RuntimeError::Fault {
        message: "fault.subscription.notOwned".into(),
        code: None,
    }
}

impl Server {
    pub(super) fn forward(&mut self, method: Method, params: Value, pending: Pending) {
        self.next_token += 1;
        self.calls.insert(self.next_token, pending);
        self.to_runtime.push_back(ClientMsg::Request {
            token: self.next_token,
            method,
            params,
        });
    }

    fn end_input(&mut self) {
        if self.input_open {
            self.input_open = false;
            self.to_runtime.push_back(ClientMsg::Eof);
        }
    }

    async fn reply(
        &mut self,
        id: Option<RequestId>,
        result: Result<Value, RuntimeError>,
    ) -> Result<()> {
        match id {
            Some(id) => self.sink.send(vec![response(id, result)]).await,
            None => Ok(()),
        }
    }

    async fn on_input(&mut self, message: Input) -> Result<()> {
        let invalid = || Some(RequestId::Text("invalid-message".into()));
        match message {
            Input::Request(request) => self.on_request(request).await,
            Input::Response { id, result } => {
                self.to_runtime
                    .push_back(ClientMsg::HostReply { id, result });
                Ok(())
            }
            Input::Invalid => {
                let error = RuntimeError::Coded {
                    code: -32700,
                    message: "Invalid protocol request".into(),
                };
                self.reply(invalid(), Err(error)).await
            }
            Input::TooLarge => {
                let error = RuntimeError::Coded {
                    code: -32600,
                    message: "Request exceeds size limit".into(),
                };
                self.end_input();
                self.reply(invalid(), Err(error)).await
            }
            Input::Eof => {
                self.end_input();
                Ok(())
            }
        }
    }

    async fn on_request(&mut self, request: Request) -> Result<()> {
        let Request {
            id, method, params, ..
        } = request;
        let routed = match method.as_str() {
            "v4/conversation/subscribe" => self.subscribe(id.clone(), &params),
            "v4/conversation/resync" => self.resync(id.clone(), &params),
            "v4/conversation/unsubscribe" => self.unsubscribe(&params).map(Some),
            "v4/connection/flow" => self.flow(&params).map(Some),
            name => match Method::parse(name) {
                Some(method) => {
                    self.forward(method, params, Pending::Rpc(id.clone()));
                    Ok(None)
                }
                None => Err(RuntimeError::MethodNotFound(name.into())),
            },
        };
        match routed {
            Ok(None) => Ok(()),
            Ok(Some(value)) => self.reply(id, Ok(value)).await,
            Err(error) => self.reply(id, Err(error)).await,
        }
    }
}
