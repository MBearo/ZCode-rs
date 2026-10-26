//! 单写者（docs/specs/rust-cli/app-protocol.md §3.3）：一个任务按提交的顺序逐个跑异步的工作。要保证“先算出的帧先写出”
//! 的地方（同一个主题的首帧与增量）都经它，锁只在计算时短暂持有，写出时不持锁（本工程不许跨 `.await` 持锁）。

use futures_util::future::BoxFuture;
use rust_cli_base::{Spawner, Task};
use tokio::sync::mpsc;

/// 一件工作。
pub type Job = BoxFuture<'static, ()>;

/// 按顺序跑工作的任务。丢弃时任务随之结束（排着的工作不再跑）。
pub struct Serial {
    jobs: mpsc::UnboundedSender<Job>,
    _task: Task<()>,
}

impl Serial {
    pub fn new(spawner: &Spawner, name: &'static str) -> Self {
        let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
        let task = spawner.spawn(name, async move {
            while let Some(job) = queue.recv().await {
                job.await;
            }
        });
        Self { jobs, _task: task }
    }

    /// 排一件工作（不等它跑完）。任务已经结束时丢弃。
    pub fn run(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

#[cfg(test)]
#[path = "serial_tests.rs"]
mod tests;
