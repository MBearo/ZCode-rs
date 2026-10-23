//! `/goal`：停止前检查的目标与 `goal_status` attachment（docs/specs/rust-cli/goal.md）。
//!
//! 目标的唯一 owner 是会话 actor；对话里的 `goal_status` attachment 是持久事实（resume 时从它恢复）。
//! 这里只有形状与来源名，状态变化与文字在 session，评估在 hooks。

use serde::{Deserialize, Serialize};

/// `goal_status` attachment 的来源（attachment type）。文本为空，发给模型时不渲染。
pub const STATUS_SOURCE: &str = "goal_status";
/// 设定目标时给模型的说明的 meta 来源。
pub const PROMPT_SOURCE: &str = "goal";

/// `goal_status` attachment 的数据（键名固定，resume 时读回）。`sentinel` 是设定 / 清除时的标记，不显示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalStatus {
    pub met: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sentinel: bool,
    /// 条件被判为不可能。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    pub condition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// 正在生效的目标的快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGoal {
    pub condition: String,
    /// 已评估且未达成的次数。
    pub iterations: u32,
    pub set_at_ms: u64,
    /// 设定以来主线程的输出 token。
    pub tokens: u64,
    /// 上一次评估给出的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
}

/// `/goal` 面板的数据（`SessionHandle::goal`）：当前目标，以及最近一次达成的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ActiveGoal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved: Option<GoalStatus>,
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
