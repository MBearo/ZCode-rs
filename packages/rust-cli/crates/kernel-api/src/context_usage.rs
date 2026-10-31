//! 上下文用量（`/context`，docs/specs/rust-cli/context-usage.md）：前端的查询与内核给出的结构化结果，以及
//! 数字的写法（`-p` 的 markdown 与 TUI 的方格图共用）。

use serde::{Deserialize, Serialize};

use crate::env::WorkspaceRef;
use crate::ids::SessionId;
use crate::model::ModelSelection;
use crate::permission::PermissionMode;

/// `Kernel::context_usage` 的参数。没有会话时按新会话的模型与模式算（不建会话）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsageQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub workspace: WorkspaceRef,
    pub selection: ModelSelection,
    pub mode: PermissionMode,
}

/// 类别的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    Used,
    Free,
    Buffer,
    /// 只按名字列出、不占上下文的延迟工具。
    Deferred,
}

/// 一个类别（名字是固定的英文，例如 `System prompt`、`Free space`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCategory {
    pub name: String,
    pub tokens: u64,
    pub kind: CategoryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolUsage {
    pub name: String,
    pub server: String,
    pub tokens: u64,
}

/// 规则文件（memory files）。`kind` 是 `User`、`Project` 等。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileUsage {
    pub path: String,
    pub kind: String,
    pub tokens: u64,
}

/// 自定义 agent；`source` 是显示名（`User`、`Project`、`Plugin`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentUsage {
    pub agent_type: String,
    pub source: String,
    pub tokens: u64,
}

/// 一个 skill 的清单行估算；`source` 是显示名（`Built-in`、`User`、`Project`、`Managed`、`Plugin`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUsage {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    pub tokens: u64,
}

/// 窗口从哪里来（compact.md §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowSource {
    Env,
    ModelDefault,
    UnknownModel,
    Auto,
}

/// 一次 `/context` 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub model: String,
    pub total_tokens: u64,
    /// 窗口。
    pub raw_max_tokens: u64,
    pub percentage: u64,
    pub window_source: WindowSource,
    pub auto_compact: bool,
    /// 顺序：使用中的类别、缓冲、`Free space`。
    pub categories: Vec<UsageCategory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<McpToolUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_files: Vec<MemoryFileUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillUsage>,
}

impl ContextUsage {
    /// 超限说明；没有超出窗口时为 None。
    pub fn over_limit(&self) -> Option<String> {
        if self.total_tokens <= self.raw_max_tokens {
            return None;
        }
        let over = format_tokens(self.total_tokens - self.raw_max_tokens);
        let window = format_tokens(self.raw_max_tokens);
        Some(if self.window_source == WindowSource::Auto {
            let run = if self.auto_compact {
                "/compact or /clear"
            } else {
                "/clear"
            };
            format!(
                "Context exceeds the {window}-token limit by {over} tokens \u{2014} run {run} to continue."
            )
        } else {
            let run = if self.auto_compact {
                "/compact"
            } else {
                "/clear"
            };
            format!(
                "Context is {over} tokens past the {window}-token compaction window \u{2014} run {run} to reduce usage."
            )
        })
    }
}

/// compact 写法每一级是上一级的这么多倍（`k`、`m`、`b`、`t`）。
pub(crate) const COMPACT_STEP: u64 = 1_000;
/// 保留一位小数：乘它、取整、再除以它。
pub(crate) const TENTHS: f64 = rust_cli_base::units::TENTHS_F64;

/// token 数的简写：小于 1000 时最多一位小数；否则 Intl 的 compact 写法（一位小数，
/// `k` / `m` / `b` / `t` 小写），再去掉第一个 `.0`。
pub fn format_tokens(value: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mut scaled = value as f64;
    #[allow(clippy::cast_precision_loss)]
    let step = COMPACT_STEP as f64;
    let suffixes = ["", "k", "m", "b", "t"];
    let mut unit = 0;
    while scaled >= step && unit + 1 < suffixes.len() {
        scaled /= step;
        unit += 1;
    }
    // Intl 的 halfExpand 舍入；进位到 1000 时换下一个单位。
    let mut rounded = (scaled * TENTHS).round() / TENTHS;
    if unit > 0 && rounded >= step && unit + 1 < suffixes.len() {
        rounded = ((rounded / step) * TENTHS).round() / TENTHS;
        unit += 1;
    }
    let text = if unit == 0 {
        if rounded.fract() == 0.0 {
            format!("{rounded:.0}")
        } else {
            format!("{rounded:.1}")
        }
    } else {
        format!("{rounded:.1}{}", suffixes[unit])
    };
    text.replacen(".0", "", 1)
}

/// token 估计值的写法：小于 20 为 `< 20`，否则 `~` 加按 10 取整的数字。
pub fn format_estimate(tokens: u64) -> String {
    // 小于它时只写 `< 20`；取整的步长。
    const FLOOR: u64 = 20;
    const STEP: u64 = 10;
    if tokens < FLOOR {
        return format!("< {FLOOR}");
    }
    format!("~{}", format_tokens((tokens + STEP / 2) / STEP * STEP))
}

#[cfg(test)]
#[path = "context_usage_tests.rs"]
mod tests;
