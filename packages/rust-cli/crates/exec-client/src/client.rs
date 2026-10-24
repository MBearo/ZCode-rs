//! 客户端的入口：连上执行器、发请求、开流、关闭。

use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::TaskScope;
use rust_cli_exec_transport::Connector;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::handshake::{STREAM_WINDOW, Welcome};
use rust_cli_exec_wire::op::Op;
use rust_cli_exec_wire::request::Request;
use tokio::sync::{mpsc, oneshot, watch};

use crate::connection::{Queues, connect_once, connected, supervise};
use crate::state::{Control, Pending, Shared, Waiter};
use crate::stream::ClientStream;
use crate::{Answer, ClientError, ClientOptions, ConnectionState};

/// 队列的长度：满了调用方就等（背压）。
const QUEUE: usize = 64;
/// 关闭时等任务收尾的时间。
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// 到一个执行器的客户端。可以克隆；最后一份被丢掉时连接关闭。
#[derive(Clone)]
pub struct ExecClient {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    scope: TaskScope,
}

impl ExecClient {
    /// 连上并握手；之后的断线与重连在后台处理。
    pub async fn connect(
        connector: Arc<dyn Connector>,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        let (requests, requests_rx) = mpsc::channel(QUEUE);
        let (control, control_rx) = mpsc::channel(QUEUE);
        let shared = Arc::new(Shared::new(options, requests, control));
        let live = connect_once(connector.as_ref(), &shared).await?;
        // 调用方拿到客户端时状态已经是“连上”（不等后台任务）。
        shared.state().connected = true;
        let epoch = shared.state().epoch;
        if let Some(welcome) = shared.state().welcome.clone() {
            connected(&shared, epoch, &welcome.server_instance);
        }
        let scope = TaskScope::root("exec-client", |panic| {
            tracing::error!(task = panic.task, message = %panic.message, "executor client task panicked");
        });
        let queues = Queues {
            requests: requests_rx,
            control: control_rx,
        };
        // 任务属于作用域：丢掉句柄不影响它。
        drop(scope.spawner().spawn(
            "connection",
            supervise(shared.clone(), connector, live, queues),
        ));
        Ok(Self {
            inner: Arc::new(Inner { shared, scope }),
        })
    }

    /// 最近一次握手的结果（执行器的系统、架构、能力位）。
    pub fn welcome(&self) -> Option<Welcome> {
        self.inner.shared.state().welcome.clone()
    }

    /// 连接现在是否就绪。
    pub fn is_connected(&self) -> bool {
        self.inner.shared.state().connected
    }

    /// 订阅连接的状态（§19.8）：连上、断开重连、关闭。
    pub fn states(&self) -> watch::Receiver<ConnectionState> {
        self.inner.shared.connection.subscribe()
    }

    /// 一次请求，等它的回复。
    pub async fn call(&self, request: Request, body: Body) -> Result<Answer, ClientError> {
        let (tx, rx) = oneshot::channel();
        let call = self.register(request, body, Waiter::Unary(tx), false)?;
        self.enqueue(call).await?;
        rx.await.map_err(|_| ClientError::Closed)?
    }

    /// 开一个流（`watch_command`、`shell_notices`）。
    pub async fn stream(&self, request: Request, body: Body) -> Result<ClientStream, ClientError> {
        let capacity = usize::try_from(STREAM_WINDOW).unwrap_or(64) + 1;
        let (tx, rx) = mpsc::channel(capacity);
        let call = self.register(request, body, Waiter::Stream { tx, received: 0 }, true)?;
        self.enqueue(call).await?;
        Ok(ClientStream::new(rx, call, self.inner.shared.clone()))
    }

    /// 关闭：关掉连接的发送方向，等任务收尾。之后的调用都是 [`ClientError::Closed`]。
    pub async fn close(&self) {
        let shared = &self.inner.shared;
        shared.state().closed = Some(ClientError::Closed);
        shared.shutdown.cancel();
        shared.connection.send_replace(ConnectionState::Closed {
            error: ClientError::Closed,
        });
        let left = self.inner.scope.close(CLOSE_GRACE).await;
        if !left.is_empty() {
            tracing::warn!(target: "exec_client", ?left, "executor client tasks were aborted");
        }
        shared.state().fail_all(&ClientError::Closed);
    }

    fn register(
        &self,
        request: Request,
        body: Body,
        waiter: Waiter,
        stream: bool,
    ) -> Result<u64, ClientError> {
        let op: Op = request.op();
        if op.is_stream() != stream {
            return Err(ClientError::Protocol(format!(
                "{} is {} a stream",
                op.as_str(),
                if stream { "not" } else { "" }
            )));
        }
        let mut state = self.inner.shared.state();
        if let Some(closed) = &state.closed {
            return Err(closed.clone());
        }
        let supported = state
            .welcome
            .as_ref()
            .is_some_and(|welcome| welcome.capabilities.contains(op.capability()));
        if !supported {
            return Err(ClientError::Unsupported {
                what: op.as_str().to_owned(),
            });
        }
        let call = state.next_call;
        state.next_call += 1;
        state.pending.insert(
            call,
            Pending {
                request,
                body,
                waiter,
                sent_epoch: 0,
            },
        );
        Ok(call)
    }

    async fn enqueue(&self, call: u64) -> Result<(), ClientError> {
        if self.inner.shared.requests.send(call).await.is_err() {
            let mut state = self.inner.shared.state();
            state.pending.remove(&call);
            return Err(state.closed.clone().unwrap_or(ClientError::Closed));
        }
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // 没有显式关闭：取消后台任务（作用域随之释放），传输随任务一起被丢掉。
        self.shared.shutdown.cancel();
    }
}

/// 不再要一个流（[`ClientStream`] 被丢掉时调用）。
pub(crate) fn abandon(shared: &Shared, call: u64) {
    if shared.state().pending.remove(&call).is_some() {
        let _ = shared.control.try_send(Control::Close(call));
    }
}
