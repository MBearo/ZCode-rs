// 复制自 rust/crates/async/src/owned.rs（feat/rs 79b6691c55）。
//! 由句柄拥有的任务与阻塞调用（docs/specs/rust-u2-task-scope.md §2、§3）。
//!
//! 在同一个函数里启动并等待的结构化任务不需要作用域：句柄就是它的所有者。丢弃句柄即中止
//! 任务，移动句柄即转移所有权；句柄的持有者本身属于某个作用域，所以任务同样不会比作用域
//! 活得更久。

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::task::JoinHandle;

use crate::{Panicked, TaskError, catch_panic};

/// 启动一个由返回的句柄拥有的任务。丢弃句柄即中止任务；等待句柄得到任务的结果，panic 变成
/// [`TaskError::Panicked`]，不向等待方传播。
///
/// panic 只交给等待句柄的一方：从不被等待、只会被中止的任务里不要放可能 panic 的逻辑。
pub fn owned<F>(future: F) -> OwnedTask<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    OwnedTask(tokio::spawn(catch_panic(future)))
}

/// [`owned`] 的句柄。
pub struct OwnedTask<T>(JoinHandle<Result<T, Panicked>>);

impl<T> OwnedTask<T> {
    /// 中止任务；之后等待句柄得到 [`TaskError::Aborted`]（任务已经结束时仍得到它的结果）。
    pub fn abort(&self) {
        self.0.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Future for OwnedTask<T> {
    type Output = Result<T, TaskError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0)
            .poll(context)
            .map(|joined| match joined {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(panicked)) => Err(TaskError::Panicked(panicked)),
                // 任务体的 panic 已在内部捕获，正常只会因为被中止而走到 JoinError。
                Err(error) if error.is_panic() => Err(TaskError::Panicked(Panicked::from_payload(
                    error.into_panic(),
                ))),
                Err(_) => Err(TaskError::Aborted),
            })
    }
}

/// 在阻塞线程池里运行同步代码并等待结果；panic 变成 [`TaskError::Panicked`]。
///
/// 工作在第一次轮询时启动。丢弃返回的 future 只是不再等待：已经开始的阻塞工作无法停止，
/// 会一直运行到结束（需要提前结束的工作自己检查取消令牌）。
pub async fn blocking<F, T>(work: F) -> Result<T, TaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(move || std::panic::catch_unwind(AssertUnwindSafe(work)))
        .await
    {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(payload)) => Err(TaskError::Panicked(Panicked::from_payload(payload))),
        // 运行时关闭时尚未开始的阻塞工作被取消。
        Err(_) => Err(TaskError::Aborted),
    }
}
