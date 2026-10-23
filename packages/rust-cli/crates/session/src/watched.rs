//! actor 直接持有的外部流：执行环境的后台命令通知、子代理信号、定时任务唤醒。
//!
//! 审查 R1-20：原来 `notices`、`agent_signals`、`cron` 各有一份相同的 `Option<Mutex<BoxStream>>` 包装，
//! 合成这一个泛型。

use std::sync::{Mutex, PoisonError};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;

/// 一个外部流。只在 actor 自己的任务里经 `&mut` 访问（`get_mut`，不加锁）；包一层 Mutex 只是让 Actor 仍是
/// `Sync`——actor 的方法以 `&self` 跨 await 调用。
pub(crate) struct Watched<T>(Option<Mutex<BoxStream<'static, T>>>);

impl<T> Watched<T> {
    pub(crate) fn new(stream: BoxStream<'static, T>) -> Self {
        Self(Some(Mutex::new(stream)))
    }

    /// 下一项；流结束或经 [`Watched::stop`] 停用后永远等待（`select!` 里这一支不再就绪）。
    pub(crate) async fn next(&mut self) -> T {
        if let Some(stream) = &mut self.0 {
            let stream = stream.get_mut().unwrap_or_else(PoisonError::into_inner);
            if let Some(item) = stream.next().await {
                return item;
            }
            self.0 = None;
        }
        std::future::pending().await
    }

    /// 以后不再等这个流（例如执行环境不提供通知）。
    pub(crate) fn stop(&mut self) {
        self.0 = None;
    }
}

#[cfg(test)]
#[path = "watched_tests.rs"]
mod tests;
