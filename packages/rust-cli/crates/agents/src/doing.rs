//! 子代理的当前活动（docs/specs/rust-cli/stall-visibility.md §4.3）：子循环事件的投影，只在内存，供输入框下面的
//! 子代理列表显示“在等重试 / 流停住了”。后台子代理没有父工具调用，它的事件到不了界面，所以由
//! 子代理状态的 owner（`live::Live`）记这一份；结束运行时随运行状态一起失效（`live.rs` 的 `settle_times`）。
//!
//! 修复线 F26（用户决定）：不再有“正在推理”。原来推理增量记成 `Thinking`，列表行尾在 `∴ Thinking` 与工具名之间
//! 来回切换；推理增量现在与文字增量相同：没有活动（kernel-api 的 `SubagentActivity::Thinking` 只加不改，保留不用）。
//!
//! 修复线 F62（用户决定）：也不再有“正在调用工具”。原来 `ToolStarted` 记成 `Tool{name}`，Explore 的 Bash 只跑几十
//! 毫秒，列表行描述后面的 ` · Bash` 一闪而过；工具调用现在与增量相同：没有活动（`SubagentActivity::Tool` 同样保留不用）。

use rust_cli_kernel_api::model::ModelNotice;
use rust_cli_kernel_api::orchestration::QueryEvent;
use rust_cli_kernel_api::panels::SubagentActivity;

/// 活动本身（不含时刻）。同一种活动的增量不算变化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Doing {
    Retrying {
        attempt: u32,
        max_retries: u32,
        delay_ms: u64,
        error: String,
    },
    Stalled {
        retry_in_ms: u64,
    },
}

impl Doing {
    /// 这个事件之后的活动；外层 None 表示事件不改变活动（工具进度）。
    pub(crate) fn after(event: &QueryEvent) -> Option<Option<Self>> {
        Some(match event {
            // 推理、文字与工具参数的增量、工具调用：没有活动（F26 去掉“正在推理”，F62 去掉工具名）。
            QueryEvent::Delta { .. }
            | QueryEvent::ToolStarted { .. }
            | QueryEvent::ToolFinished { .. } => None,
            QueryEvent::ToolProgress { .. } => return None,
            QueryEvent::ModelNotice { notice } => match notice {
                ModelNotice::Retrying {
                    attempt,
                    max_retries,
                    delay_ms,
                    error,
                } => Some(Self::Retrying {
                    attempt: *attempt,
                    max_retries: *max_retries,
                    delay_ms: *delay_ms,
                    error: error.to_string(),
                }),
                ModelNotice::Stalled { retry_in_ms } => Some(Self::Stalled {
                    retry_in_ms: *retry_in_ms,
                }),
                ModelNotice::Resumed => None,
            },
        })
    }

    /// 快照里的样子：重试与停顿换算成查询时刻还要等多久（`since_ms` 是记下活动后过了多久）。
    pub(crate) fn shown(&self, since_ms: u64) -> SubagentActivity {
        match self {
            Self::Retrying {
                attempt,
                max_retries,
                delay_ms,
                error,
            } => SubagentActivity::Retrying {
                attempt: *attempt,
                max_retries: *max_retries,
                retry_in_ms: delay_ms.saturating_sub(since_ms),
                error: error.clone(),
            },
            Self::Stalled { retry_in_ms } => SubagentActivity::Stalled {
                retry_in_ms: retry_in_ms.saturating_sub(since_ms),
            },
        }
    }
}
