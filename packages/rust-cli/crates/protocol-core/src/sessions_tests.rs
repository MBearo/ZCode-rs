use std::sync::Mutex as StdMutex;

use futures_util::stream::{self, BoxStream};
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::session_api::{Ack, CommandEnvelope, CreateSession, KernelEvent};
use rust_cli_kernel_api::store::{SessionFilter, SessionSummary};
use rust_cli_kernel_api::testing::selection;

use super::*;

fn title(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: KernelEvent::TitleChanged {
            title: format!("t{seq}"),
        },
        revision: seq,
    }
}

/// 一个会话：订阅时给出 `after` 之后已有的事件；第一次订阅的流只给前两个（模拟落后后流结束）。
struct FakeHandle {
    id: SessionId,
    events: Vec<SequencedEvent>,
    subscriptions: StdMutex<Vec<Option<u64>>>,
}

#[async_trait]
impl SessionHandle for FakeHandle {
    fn id(&self) -> &SessionId {
        &self.id
    }

    async fn send(&self, _command: CommandEnvelope) -> Ack {
        Ack::Duplicate
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.push(after);
        let after = after.unwrap_or(0);
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.seq > after)
            .cloned()
            .collect();
        let first = subscriptions.len() == 1;
        stream::iter(if first {
            events.into_iter().take(2).collect()
        } else {
            events
        })
        .boxed()
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Err(KernelError::Unsupported {
            what: "view".to_owned(),
        })
    }
}

struct FakeKernel {
    handle: Arc<FakeHandle>,
    opens: StdMutex<u32>,
}

#[async_trait]
impl Kernel for FakeKernel {
    async fn create_session(&self, _request: CreateSession) -> Result<SessionId, KernelError> {
        Err(KernelError::Unsupported {
            what: "create".to_owned(),
        })
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        *self.opens.lock().unwrap() += 1;
        if session != &self.handle.id {
            return Err(KernelError::NotFound {
                session: session.clone(),
            });
        }
        Ok(self.handle.clone())
    }

    async fn list_sessions(
        &self,
        _filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }
}

/// 接收方记下事件与每次 flush 时累计的个数。
#[derive(Default)]
struct Recorder {
    seqs: Mutex<Vec<u64>>,
    flushes: Mutex<Vec<usize>>,
}

#[async_trait]
impl SessionSink for Recorder {
    async fn event(&self, event: SequencedEvent) {
        self.seqs.lock().unwrap().push(event.seq);
    }

    async fn flush(&self) {
        let count = self.seqs.lock().unwrap().len();
        self.flushes.lock().unwrap().push(count);
    }
}

fn view(last_seq: u64) -> SessionView {
    SessionView {
        meta: rust_cli_kernel_api::store::SessionMeta {
            id: SessionId::new("s1"),
            kind: rust_cli_kernel_api::store::SessionKind::Interactive,
            parent: None,
            workspace: rust_cli_kernel_api::env::WorkspaceRef {
                environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
                root: rust_cli_kernel_api::env::EnvPath::new("/w"),
            },
            title: String::new(),
            trace: rust_cli_kernel_api::ids::TraceId::new("trace"),
            created_at_ms: 0,
        },
        messages: Vec::new(),
        selection: selection(),
        mode: rust_cli_kernel_api::permission::PermissionMode::Default,
        plan: false,
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq,
        revision: 0,
        running: false,
        background: Vec::new(),
        peer_held: Vec::new(),
        context_usage: None,
        context_notice: Default::default(),
        output_speed: None,
    }
}

/// 视图里的 `last_seq` 之后的事件都交给接收方：流结束后从最后的 seq 接着订阅，不重不漏；一个窗口里的事件
/// 一起 flush。会话只打开一次。
#[tokio::test]
async fn events_after_the_view_reach_the_sink_once_even_when_the_stream_restarts() {
    let handle = Arc::new(FakeHandle {
        id: SessionId::new("s1"),
        events: (1..=5).map(title).collect(),
        subscriptions: StdMutex::new(Vec::new()),
    });
    let kernel = Arc::new(FakeKernel {
        handle: handle.clone(),
        opens: StdMutex::new(0),
    });
    let scope = TaskScope::root("test", |_| {});
    let bridge =
        SessionBridge::<Recorder>::new(kernel.clone(), scope.spawner(), Duration::from_millis(30));
    let id = SessionId::new("s1");
    // 视图不可用时打开失败（FakeHandle 的 view 返回 Unsupported）：用一个给出视图的包装。
    assert!(bridge.open(&id, |_, _| Recorder::default()).await.is_err());
    let bridge = SessionBridge::<Recorder>::new(
        Arc::new(ViewKernel(kernel.clone())),
        scope.spawner(),
        Duration::from_millis(30),
    );
    let first = bridge.open(&id, |_, _| Recorder::default()).await.unwrap();
    let again = bridge.open(&id, |_, _| Recorder::default()).await.unwrap();
    assert!(Arc::ptr_eq(&first.sink, &again.sink));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*first.sink.seqs.lock().unwrap(), vec![2, 3, 4, 5]);
    // 排进泵任务的工作在事件之后跑。
    let (done, wait) = tokio::sync::oneshot::channel();
    let sink = first.sink.clone();
    first.run(Box::pin(async move {
        let _ = done.send(sink.seqs.lock().unwrap().len());
    }));
    assert_eq!(wait.await.unwrap(), 4);
    // 第一个流给了 2、3 就结束；第二个从 3 之后接；之后两次空流就停。
    assert_eq!(
        *handle.subscriptions.lock().unwrap(),
        vec![Some(1), Some(3), Some(5), Some(5)]
    );
    assert!(!first.sink.flushes.lock().unwrap().is_empty());
    assert_eq!(*first.sink.flushes.lock().unwrap().last().unwrap(), 4);
    assert!(bridge.close(&id).is_some());
    assert!(bridge.get(&id).is_none());
}

/// 给 FakeHandle 补上视图（last_seq = 1）。
struct ViewKernel(Arc<FakeKernel>);

struct ViewHandle(Arc<FakeHandle>);

#[async_trait]
impl SessionHandle for ViewHandle {
    fn id(&self) -> &SessionId {
        self.0.id()
    }

    async fn send(&self, command: CommandEnvelope) -> Ack {
        self.0.send(command).await
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        self.0.subscribe(after)
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Ok(view(1))
    }
}

#[async_trait]
impl Kernel for ViewKernel {
    async fn create_session(&self, request: CreateSession) -> Result<SessionId, KernelError> {
        self.0.create_session(request).await
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.0.open_session(session).await?;
        Ok(Arc::new(ViewHandle(self.0.handle.clone())))
    }

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        self.0.list_sessions(filter).await
    }
}
