//! 模型请求的重试与停顿通知（docs/specs/rust-cli/stall-visibility.md §3.2）：内核事件 `ModelNotice`（主线程）
//! 与子循环事件 `QueryEvent::ModelNotice`（前台子代理的 Agent 行）的投影。只记最后一条通知与前端收到它的时刻；
//! 倒计时只用于显示，不参与状态判断。
use rust_cli_kernel_api::model::ModelNotice;

use super::Transcript;

/// 一条还在生效的通知（`Retrying` 或 `Stalled`）与收到它的时刻（毫秒，前端的时钟）。
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub notice: ModelNotice,
    pub at_ms: u64,
}

impl Notice {
    /// 折叠一条通知：`Resumed` 清掉前面的通知，其余替换它。
    pub(crate) fn fold(slot: &mut Option<Self>, notice: ModelNotice, now_ms: u64) {
        *slot = match notice {
            ModelNotice::Resumed => None,
            notice => Some(Self {
                notice,
                at_ms: now_ms,
            }),
        };
    }

    /// 离重发（或看门狗中止）还有多久，不小于 0。
    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        let total = match &self.notice {
            ModelNotice::Retrying { delay_ms, .. } => *delay_ms,
            ModelNotice::Stalled { retry_in_ms } => *retry_in_ms,
            ModelNotice::Resumed => 0,
        };
        total.saturating_sub(now_ms.saturating_sub(self.at_ms))
    }
}

impl Transcript {
    /// 主线程还在生效的重试或停顿通知（状态行据此换成重试行）。
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
