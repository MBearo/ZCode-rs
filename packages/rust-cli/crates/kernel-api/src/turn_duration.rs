//! 回合结束那一行的数据（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）。
//!
//! 会话 actor 是用时的唯一来源：回合开始到结束，扣掉回合里有待回答的交互的时间；等后台子代理之后的那一行从第一个等待的
//! 回合开始算。前端只排版，不另计时。

use serde::{Deserialize, Serialize};

use crate::ids::TurnId;

/// 一个回合结束那一行：[`crate::session_api::KernelEvent::TurnDuration`] 的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnDuration {
    pub turn: TurnId,
    /// 用时（毫秒）。`waiting_agents` 大于 0 时前端不显示它。
    pub duration_ms: u64,
    /// 回合结束的时刻（Unix 毫秒，内核的时钟）。
    pub finished_at_ms: u64,
    /// 内核的时钟在 `finished_at_ms` 那一刻相对 UTC 的偏移（分钟，东正西负）：前端据此写本地的时刻。
    #[serde(default)]
    pub utc_offset_minutes: i32,
    /// 回合结束时还在跑、或已结束但通知还没送达的后台子代理数；大于 0 时前端写“等它们完成”。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waiting_agents: u32,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde 的 skip_serializing_if 按引用传字段"
)]
fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
#[path = "turn_duration_tests.rs"]
mod tests;
