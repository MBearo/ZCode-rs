//! 一条连接（docs/specs/rust-cli/app-protocol.md §3.3、§4.1）：按 LF 分帧的读循环、请求派发、回复与通知的写者、本端发出的
//! 请求（反向请求）的关联。
//!
//! - 读循环只做解析与派发：每个请求在自己的任务里处理，回复按完成顺序写出；读循环不等请求处理完。
//! - 写出经一个写者任务串行化，一行一个完整的 JSON；队列有界，满了时写的一方等（不丢）。
//! - 请求的回复可以带一个“回复之后”的动作（例如订阅之后发首帧）：动作在回复写进队列之后才开始，所以它写的东西排在回复
//!   之后。
//! - 本端发的请求用字符串 id `rs-<n>`，与对端的 id 空间分开；连接断开时等待中的请求全部失败。不靠超时同步状态。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rust_cli_base::{Spawner, Task, units};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::rpc::{self, Incoming, RpcError};

/// 连接断开后发出的请求得到的错误码（实现自定义的范围）。
pub const CONNECTION_CLOSED: i64 = -32000;

/// 回复之后要做的事（排在回复之后写出）。
pub type After = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// 一个请求的结果：回复的 `result`，以及回复写出之后的动作。
pub struct Reply {
    pub result: Value,
    pub then: Option<After>,
}

impl Reply {
    pub fn new(result: Value) -> Self {
        Self { result, then: None }
    }

    pub fn then(result: Value, after: After) -> Self {
        Self {
            result,
            then: Some(after),
        }
    }
}

impl From<Value> for Reply {
    fn from(result: Value) -> Self {
        Self::new(result)
    }
}

/// 协议的处理方。
#[async_trait]
pub trait Service: Send + Sync + 'static {
    async fn request(&self, method: &str, params: Value) -> Result<Reply, RpcError>;

    /// 对端的通知。缺省忽略（不认识的通知一律忽略）。
    async fn notification(&self, method: &str, params: Value) {
        let _ = (method, params);
    }
}

/// 缺省：一行的上限（字节）。
const DEFAULT_MAX_LINE_BYTES: usize = 64 * units::MIB;
/// 缺省：写者队列的容量（行）。
const DEFAULT_QUEUE: usize = 1024;

/// 连接的参数。
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 一行的上限（字节）；超过的行丢弃并写诊断。
    pub max_line_bytes: usize,
    /// 写者队列的容量（行）。
    pub queue: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            queue: DEFAULT_QUEUE,
        }
    }
}

type Waiting = HashMap<String, oneshot::Sender<Result<Value, RpcError>>>;

struct PeerInner {
    out: mpsc::Sender<String>,
    waiting: Mutex<Waiting>,
    next: AtomicU64,
    closed: AtomicBool,
}

/// 往对端写东西的句柄：通知、回复与本端发的请求。可克隆。
#[derive(Clone)]
pub struct Peer(Arc<PeerInner>);

impl Peer {
    /// 写一行（不带换行）。连接已断开时返回假。
    pub async fn write(&self, line: String) -> bool {
        self.0.out.send(line).await.is_ok()
    }

    pub async fn notify(&self, method: &str, params: &Value) -> bool {
        self.write(rpc::notification(method, params)).await
    }

    /// 不等队列的通知（在不能 await 的地方用，例如取消时的 Drop）：队列满或已断开时丢弃并返回假。
    pub fn try_notify(&self, method: &str, params: &Value) -> bool {
        self.0
            .out
            .try_send(rpc::notification(method, params))
            .is_ok()
    }

    /// 发一个请求并等回复。连接断开时（之前或等待中）返回 [`CONNECTION_CLOSED`]。
    pub async fn request(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        let closed = || RpcError::new(CONNECTION_CLOSED, "the connection is closed");
        if self.0.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        let id = format!("rs-{}", self.0.next.fetch_add(1, Ordering::Relaxed) + 1);
        let (sender, receiver) = oneshot::channel();
        self.waiting().insert(id.clone(), sender);
        // 等待表登记之后再检查一次：读循环在两者之间结束时也不会漏掉。
        if self.0.closed.load(Ordering::Acquire) {
            self.waiting().remove(&id);
            return Err(closed());
        }
        let guard = Forget {
            peer: self.clone(),
            id: id.clone(),
        };
        if !self
            .write(rpc::request(&Value::String(id), method, params))
            .await
        {
            return Err(closed());
        }
        let result = receiver.await.unwrap_or_else(|_| Err(closed()));
        drop(guard);
        result
    }

    /// 连接已经断开（读到结束或写失败）。
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }

    fn waiting(&self) -> std::sync::MutexGuard<'_, Waiting> {
        self.0
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn resolve(&self, id: &Value, result: Result<Value, RpcError>) {
        let Some(key) = id.as_str() else {
            tracing::warn!(
                event = "protocol.response.unknown",
                "a response with a non-string id"
            );
            return;
        };
        match self.waiting().remove(key) {
            Some(sender) => {
                let _ = sender.send(result);
            }
            None => tracing::warn!(
                event = "protocol.response.unknown",
                id = key,
                "a response to no waiting request"
            ),
        }
    }

    fn close(&self) {
        self.0.closed.store(true, Ordering::Release);
        let waiting: Vec<_> = self.waiting().drain().collect();
        for (_, sender) in waiting {
            let _ = sender.send(Err(RpcError::new(
                CONNECTION_CLOSED,
                "the connection is closed",
            )));
        }
    }
}

/// 请求的 future 被丢弃（调用方取消）时从等待表里拿走。
struct Forget {
    peer: Peer,
    id: String,
}

impl Drop for Forget {
    fn drop(&mut self) {
        self.peer.waiting().remove(&self.id);
    }
}

/// 起写者任务：一行一个 JSON，写完一行就 flush。写失败时连接算断开。
pub fn start<W>(writer: W, spawner: &Spawner, options: Options) -> (Peer, Task<()>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut lines) = mpsc::channel::<String>(options.queue.max(1));
    let peer = Peer(Arc::new(PeerInner {
        out,
        waiting: Mutex::new(HashMap::new()),
        next: AtomicU64::new(0),
        closed: AtomicBool::new(false),
    }));
    let closing = peer.clone();
    let task = spawner.spawn("protocol-writer", async move {
        let mut writer = writer;
        while let Some(mut line) = lines.recv().await {
            line.push('\n');
            let written = async {
                writer.write_all(line.as_bytes()).await?;
                writer.flush().await
            };
            if let Err(error) = written.await {
                tracing::warn!(event = "protocol.write.failed", error = %error, "writing to the peer failed");
                closing.close();
                break;
            }
        }
    });
    (peer, task)
}

/// 读到的一行：内容，或者超过上限（已丢弃到行尾）。
enum Line {
    Text(String),
    TooLong,
}

/// 读一行（去掉 LF 与它前面的 CR）。读到结束时返回 None。
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<Option<Line>> {
    let mut buffer = Vec::new();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buffer.is_empty() && !too_long {
                return Ok(None);
            }
            break;
        }
        let (chunk, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..at], Some(at + 1)),
            None => (available, None),
        };
        if !too_long && buffer.len() + chunk.len() > max {
            too_long = true;
            buffer = Vec::new();
        }
        if !too_long {
            buffer.extend_from_slice(chunk);
        }
        let consumed = done.unwrap_or(available.len());
        reader.consume(consumed);
        if done.is_some() {
            break;
        }
    }
    if too_long {
        return Ok(Some(Line::TooLong));
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    Ok(Some(Line::Text(
        String::from_utf8_lossy(&buffer).into_owned(),
    )))
}

/// 读循环：一直读到对端关掉输入。结束时连接算断开，等待中的反向请求全部失败。
pub async fn serve<R>(
    mut reader: R,
    peer: Peer,
    service: Arc<dyn Service>,
    spawner: Spawner,
    options: Options,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    let result = loop {
        let line = match read_line(&mut reader, options.max_line_bytes).await {
            Ok(Some(Line::Text(line))) => line,
            Ok(Some(Line::TooLong)) => {
                tracing::warn!(
                    event = "protocol.line.too_long",
                    limit = options.max_line_bytes,
                    "an incoming line exceeded the limit and was dropped"
                );
                continue;
            }
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        };
        if line.trim().is_empty() {
            continue;
        }
        match rpc::parse(&line) {
            Ok(Incoming::Request { id, method, params }) => {
                let (peer, service) = (peer.clone(), service.clone());
                drop(spawner.spawn("protocol-request", async move {
                    let reply = service.request(&method, params).await;
                    let (result, then) = match reply {
                        Ok(Reply { result, then }) => (Ok(result), then),
                        Err(error) => (Err(error), None),
                    };
                    if !peer.write(rpc::response(&id, &result)).await {
                        return;
                    }
                    if let Some(then) = then {
                        then().await;
                    }
                }));
            }
            Ok(Incoming::Notification { method, params }) => {
                let service = service.clone();
                drop(spawner.spawn("protocol-notification", async move {
                    service.notification(&method, params).await;
                }));
            }
            Ok(Incoming::Response { id, result }) => peer.resolve(&id, result),
            Err(malformed) => match malformed.id {
                Some(id) => {
                    peer.write(rpc::response(&id, &Err(malformed.error))).await;
                }
                None => tracing::warn!(
                    event = "protocol.line.malformed",
                    error = %malformed.error,
                    "an incoming line could not be parsed"
                ),
            },
        }
    };
    peer.close();
    result
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
