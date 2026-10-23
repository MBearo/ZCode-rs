// 复制自 rust/crates/async/src/scope.rs（feat/rs 79b6691c55）。
//! 任务作用域（docs/specs/rust-u2-task-scope.md）。
//!
//! 每个任务属于一个作用域：所有者释放作用域时任务被中止，panic 被捕获并上报，关闭时能列出
//! 还在运行的任务。这里是 `tokio::spawn` 与 `spawn_blocking` 的唯一入口。

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{Panicked, catch_panic};

/// 一次任务 panic：发生在哪个作用域的哪个任务里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPanic {
    pub scope: String,
    pub task: &'static str,
    pub message: String,
}

/// 任务没有产出结果的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    Panicked(Panicked),
    /// 任务被中止：调用了 [`Task::abort`]，或它的作用域被关闭、释放。
    Aborted,
    /// 作用域在启动任务之前已经关闭，任务没有运行。
    ScopeClosed,
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panicked(panicked) => panicked.fmt(formatter),
            Self::Aborted => formatter.write_str("task aborted"),
            Self::ScopeClosed => formatter.write_str("task scope closed"),
        }
    }
}

impl std::error::Error for TaskError {}

type Report = Arc<dyn Fn(&TaskPanic) + Send + Sync>;

struct Shared {
    path: String,
    cancel: CancellationToken,
    report: Report,
    state: Mutex<State>,
    /// 整棵作用域树共用：任何任务结束时通知等待关闭的一方。
    changed: Arc<Notify>,
}

#[derive(Default)]
struct State {
    closed: bool,
    next: u64,
    tasks: HashMap<u64, Running>,
    children: Vec<Weak<Shared>>,
}

struct Running {
    name: &'static str,
    abort: AbortHandle,
}

impl Shared {
    /// 锁只保护表本身，持有期间不 await；中毒时数据仍然一致（每次修改都是单步）。
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn children(&self) -> Vec<Arc<Shared>> {
        self.state()
            .children
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// 不再接受新任务，并发出取消信号（子作用域的令牌随之取消）。
    fn stop_accepting(&self) {
        self.state().closed = true;
        self.cancel.cancel();
        for child in self.children() {
            child.stop_accepting();
        }
    }

    fn collect_running(&self, out: &mut Vec<String>) {
        out.extend(
            self.state()
                .tasks
                .values()
                .map(|task| format!("{}: {}", self.path, task.name)),
        );
        for child in self.children() {
            child.collect_running(out);
        }
    }

    fn abort_running(&self) {
        for task in self.state().tasks.values() {
            task.abort.abort();
        }
        for child in self.children() {
            child.abort_running();
        }
    }

    fn finished(&self) -> bool {
        let state = self.state();
        state.closed && state.tasks.is_empty()
    }

    fn report_panic(&self, task: &'static str, panicked: &Panicked) {
        (self.report)(&TaskPanic {
            scope: self.path.clone(),
            task,
            message: panicked.message.clone(),
        });
    }
}

/// 任务结束（完成、panic、被中止，包括一次都没被轮询就被中止）时把它从作用域里移除。
struct Done {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for Done {
    fn drop(&mut self) {
        self.shared.state().tasks.remove(&self.id);
        self.shared.changed.notify_waiters();
    }
}

/// 作用域的所有者。不可克隆；释放时中止其中（含子作用域）仍在运行的任务。
pub struct TaskScope(Arc<Shared>);

impl TaskScope {
    /// 根作用域。`report` 在任务 panic 时被调用一次，运行在出错任务所在的线程上，不能阻塞。
    pub fn root(
        name: impl Into<String>,
        report: impl Fn(&TaskPanic) + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(Shared {
            path: name.into(),
            cancel: CancellationToken::new(),
            report: Arc::new(report),
            state: Mutex::default(),
            changed: Arc::new(Notify::new()),
        }))
    }

    pub fn spawner(&self) -> Spawner {
        Spawner(self.0.clone())
    }

    /// 发出取消信号，等任务自行结束至多 `grace`，然后中止剩余的任务。
    /// 返回宽限期过后仍在运行的任务（`<作用域路径>: <任务名>`）；它们已被中止。
    pub async fn close(&self, grace: Duration) -> Vec<String> {
        self.0.stop_accepting();
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            // 先登记等待再检查，任务在两者之间结束也不会漏掉通知。
            let mut changed = std::pin::pin!(self.0.changed.notified());
            changed.as_mut().enable();
            let mut running = Vec::new();
            self.0.collect_running(&mut running);
            if running.is_empty() {
                return running;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                let mut left = Vec::new();
                self.0.collect_running(&mut left);
                self.0.abort_running();
                return left;
            }
        }
    }
}

impl Drop for TaskScope {
    fn drop(&mut self) {
        self.0.stop_accepting();
        self.0.abort_running();
    }
}

/// 在某个作用域里启动任务的权限。可克隆，可以交给任务自己；不延长作用域的寿命：
/// 作用域关闭或被所有者释放之后，经它启动的任务不会运行。
#[derive(Clone)]
pub struct Spawner(Arc<Shared>);

impl Spawner {
    /// 子作用域：路径为 `<父路径>/<name>`，取消信号随父作用域传播。
    pub fn child(&self, name: impl Into<String>) -> TaskScope {
        let child = Arc::new(Shared {
            path: format!("{}/{}", self.0.path, name.into()),
            cancel: self.0.cancel.child_token(),
            report: self.0.report.clone(),
            state: Mutex::default(),
            changed: self.0.changed.clone(),
        });
        let mut state = self.0.state();
        if state.closed {
            child.stop_accepting();
        }
        // 顺带清掉已经结束的子作用域，列表不会随连接、会话的来去无限增长。
        state
            .children
            .retain(|known| known.upgrade().is_some_and(|known| !known.finished()));
        state.children.push(Arc::downgrade(&child));
        TaskScope(child)
    }

    /// 启动一个任务。它 panic 时上报给根作用域，句柄得到 [`TaskError::Panicked`]。
    pub fn spawn<F>(&self, name: &'static str, future: F) -> Task<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.start(name, |done, shared| {
            tokio::spawn(async move {
                // `done` 由 future 持有：一次都没被轮询就被中止时也会登记结束。
                let _done = done;
                catch_panic(future).await.map_err(|panicked| {
                    shared.report_panic(name, &panicked);
                    TaskError::Panicked(panicked)
                })
            })
        })
    }

    /// 启动一个常驻任务：没有自己的收口步骤、随作用域一直运行的循环（轮询、事件转发、owner 任务）。
    /// 作用域收到取消信号（[`TaskScope::close`] 的第一步，或父作用域的取消）时，future 在当前等待点
    /// 被丢弃、任务正常结束，句柄得到 `Ok(None)`；`close` 因此不把它列为被中止的任务。对任务本身来说，
    /// 这与被中止没有区别：都是在一个等待点上丢弃 future。
    pub fn spawn_resident<F>(&self, name: &'static str, future: F) -> Task<Option<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn(
            name,
            self.0.cancel.clone().run_until_cancelled_owned(future),
        )
    }

    /// 在阻塞线程池里运行同步代码。已经开始运行的阻塞任务无法被中止。
    pub fn spawn_blocking<F, T>(&self, name: &'static str, work: F) -> Task<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.start(name, |done, shared| {
            tokio::task::spawn_blocking(move || {
                let _done = done;
                std::panic::catch_unwind(AssertUnwindSafe(work)).map_err(|payload| {
                    let panicked = Panicked::from_payload(payload);
                    shared.report_panic(name, &panicked);
                    TaskError::Panicked(panicked)
                })
            })
        })
    }

    fn start<T>(
        &self,
        name: &'static str,
        spawn: impl FnOnce(Done, Arc<Shared>) -> JoinHandle<Result<T, TaskError>>,
    ) -> Task<T> {
        // 持锁完成“启动并登记”：任务即使立刻结束，也要等登记完成才能把自己移除。
        let mut state = self.0.state();
        if state.closed {
            return Task(None);
        }
        let id = state.next;
        state.next += 1;
        let done = Done {
            shared: self.0.clone(),
            id,
        };
        let handle = spawn(done, self.0.clone());
        let abort = handle.abort_handle();
        state.tasks.insert(id, Running { name, abort });
        Task(Some(handle))
    }

    /// 作用域收到取消信号时完成；需要自行收口的任务用它。
    pub async fn cancelled(&self) {
        self.0.cancel.cancelled().await;
    }

    pub fn is_closed(&self) -> bool {
        self.0.state().closed
    }

    /// 仍在运行的任务（含子作用域），格式 `<作用域路径>: <任务名>`。用于关闭日志与测试。
    pub fn running(&self) -> Vec<String> {
        let mut running = Vec::new();
        self.0.collect_running(&mut running);
        running
    }
}

/// 任务句柄。丢弃句柄不影响任务（它仍属于作用域）；[`Task::abort`] 中止它。
pub struct Task<T>(Option<JoinHandle<Result<T, TaskError>>>);

impl<T> Task<T> {
    pub fn abort(&self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.0.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl<T> Future for Task<T> {
    type Output = Result<T, TaskError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(handle) = self.0.as_mut() else {
            return Poll::Ready(Err(TaskError::ScopeClosed));
        };
        Pin::new(handle).poll(context).map(|joined| match joined {
            Ok(result) => result,
            // 上报函数自身 panic 时才会走到这里：任务体的 panic 已在内部捕获。
            Err(error) if error.is_panic() => Err(TaskError::Panicked(Panicked::from_payload(
                error.into_panic(),
            ))),
            Err(_) => Err(TaskError::Aborted),
        })
    }
}
