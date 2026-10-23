//! 带原因的中止信号（语义同 JS `AbortController` 的 `signal.reason`）。
//!
//! 两种原因的处理不同：用户主动中断时，所有在跑的工具都回填拒绝结果并追加中断消息；运行中提交
//! 新输入引起的中断（[`AbortReason::Submit`]）只取消可中断的工具，不追加中断消息。

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    /// 用户中断（Esc / Ctrl+C）。
    User,
    /// 运行中提交了新输入。
    Submit,
    /// 进程要退出（退出信号、运行中的 `/exit`，修复线 F38，docs/specs/rust-cli/session-lifecycle.md §3.5）：
    /// 对外的效果是收到退出信号时直接退出，进程结束前不再多写东西。中止只为经取消路径回收工具；
    /// 之后这个回合不再记录：还没提交的输入不提交，不写宣布、中断标记与没写完的文字。
    Shutdown,
}

/// 可克隆的中止信号；第一次 `abort` 的原因生效。
#[derive(Debug, Clone, Default)]
pub struct Abort {
    token: CancellationToken,
    reason: Arc<OnceLock<AbortReason>>,
}

impl Abort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self, reason: AbortReason) {
        let _ = self.reason.set(reason);
        self.token.cancel();
    }

    pub fn is_aborted(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn reason(&self) -> Option<AbortReason> {
        self.reason.get().copied()
    }

    /// 底层的取消令牌（传给模型请求与工具）。
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    pub async fn aborted(&self) {
        self.token.cancelled().await;
    }
}
