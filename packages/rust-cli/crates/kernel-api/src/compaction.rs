//! 压缩的编排接口（docs/specs/rust-cli/compact.md）：请求、结果、自动压缩的运行状态与 [`Compactor`]。
//! 由 [`crate::orchestration`] 重新导出。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::env::WorkspaceRef;
use crate::ids::{AgentId, SessionId};
use crate::message::{AttachmentMessage, CompactBoundary, CompactTrigger, Message, UserMessage};
use crate::model::ModelSelection;
use crate::orchestration::{ContextRequest, OrchestrationError};
use crate::permission::PermissionMode;

/// 压缩请求（自动、手动或上下文超限后的反应式压缩）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactRequest {
    pub session: SessionId,
    pub workspace: WorkspaceRef,
    pub trigger: CompactTrigger,
    pub selection: ModelSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
    /// 摘要请求用的权限模式（工具描述随模式变化，与主线程相同才能共享缓存前缀）；缺省为 default。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    /// 子代理里的压缩带子代理 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// 触发反应式压缩的上下文超限错误给出的 token 差（由
    /// [`prompt_too_long_gap`] 解析）；压缩核心用它播种保留的组数、决定全部摘要时从头裁掉多少
    /// （docs/specs/rust-cli/compact.md §3.1）。其他触发为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_token_gap: Option<u64>,
}

/// 上下文超限错误文本里的 token 差（按正则
/// `/prompt is too long[^0-9]*(\d+)\s*tokens?\s*>\s*(\d+)/i` 取两个数，差不大于 0
/// 时为 None）。例：`prompt is too long: 1000500 tokens > 1000000 maximum` → 500。
pub fn prompt_too_long_gap(message: &str) -> Option<u64> {
    const PHRASE: &str = "prompt is too long";
    // ASCII 小写不改变字节长度，下标可以直接用在原文上。
    let start = message.to_ascii_lowercase().find(PHRASE)? + PHRASE.len();
    let number = |text: &str| -> Option<(u64, usize)> {
        let end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        Some((text[..end].parse().ok()?, end))
    };
    let rest = &message[start..];
    let rest = &rest[rest.find(|c: char| c.is_ascii_digit())?..];
    let (actual, used) = number(rest)?;
    let rest = rest[used..].trim_start();
    let rest = match rest.get(..5) {
        Some(word) if word.eq_ignore_ascii_case("token") => &rest[5..],
        _ => return None,
    };
    let rest = rest
        .strip_prefix(['s', 'S'])
        .unwrap_or(rest)
        .trim_start()
        .strip_prefix('>')?
        .trim_start();
    let (limit, _) = number(rest)?;
    actual.checked_sub(limit).filter(|gap| *gap > 0)
}

/// 输入框下方“上下文快满”的提示（docs/specs/rust-cli/compact.md
/// §2.1）。级别与百分比由压缩模块算好，前端只按变体排文字。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextNotice {
    /// 离上限还远（级别 `ok`）：不显示。
    #[default]
    Hidden,
    /// 自动压缩打开、窗口来源是 `auto`：`N% context used`（N 按有效窗口算的已用）。
    Used { percent: u64 },
    /// 自动压缩打开、窗口来源不是 `auto`：`N% until auto-compact`（N 按阈值算的剩余）。
    UntilAutoCompact { percent: u64 },
    /// 自动压缩关闭：`Context low (N% remaining)`（N 按有效窗口算的剩余）；`compact_hint` 时接
    /// ` · Run /compact to compact & continue`（`DISABLE_COMPACT` 没设，`/compact` 还能用）。
    Low { percent: u64, compact_hint: bool },
}

/// 上下文用量：token 与有效窗口（[`Compactor::context_usage`] 的结果，`KernelEvent::ContextUsage` 是同一对值；
/// docs/specs/rust-cli/compact.md §2.1、tui-display.md §2.6，修复线 F27）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextOccupancy {
    pub used_tokens: u64,
    pub max_tokens: u64,
}

impl ContextOccupancy {
    /// 占有效窗口的百分比（[`used_percent`]）。
    pub fn percent(&self) -> u64 {
        used_percent(self.used_tokens, self.max_tokens)
    }
}

/// 已用的百分比：`100 − max(0, round((W − token) / W · 100))`，与 `N% context used` 的提示是同一个式子（按 JS 的
/// 双精度算；正数时 `f64::round` 与 JS `Math.round` 相同）。窗口为 0 时是 100。footer 的 ctx 与压缩模块的
/// [`ContextNotice::Used`] 都用它。
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn used_percent(used_tokens: u64, window: u64) -> u64 {
    if window == 0 {
        return 100;
    }
    let left = (window as f64 - used_tokens as f64) / window as f64 * 100.0;
    100 - (left.round().max(0.0) as u64).min(100)
}

#[async_trait]
pub trait Compactor: Send + Sync {
    /// 这一步发请求前是否需要自动压缩。
    fn should_auto_compact(&self, history: &[Message], selection: &ModelSelection) -> bool;

    /// 上下文用量 `(token, 有效窗口)`（docs/specs/rust-cli/compact.md §2.1）：会话 actor 在提交带用量的 assistant、
    /// 压缩结果与换模型之后据此发 `KernelEvent::ContextUsage`，`SessionHandle::view` 也带它（footer 的 ctx，F27）。默认不知道。
    fn context_usage(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
    ) -> Option<(u64, u64)> {
        None
    }

    /// 上下文快满的提示（docs/specs/rust-cli/compact.md §2.1）：会话 actor 在提交带用量的 assistant 与压缩结果
    /// 之后据此发 `KernelEvent::ContextNotice`。默认不知道（不发）。
    fn context_notice(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
    ) -> Option<ContextNotice> {
        None
    }

    /// 发请求前的自动压缩：判定、熔断与压缩（`tracking` 由调用方在一次运行内持有）。默认实现只看
    /// [`should_auto_compact`](Self::should_auto_compact)，没有熔断。
    async fn auto_compact(
        &self,
        request: CompactRequest,
        tracking: &mut CompactTracking,
        cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        if !self.should_auto_compact(&request.messages, &request.selection) {
            return Ok(AutoCompactOutcome::NotNeeded);
        }
        match self.compact(request, cancel).await {
            Ok(result) => {
                tracking.compacted = true;
                tracking.turn_counter = 0;
                tracking.consecutive_failures = 0;
                Ok(AutoCompactOutcome::Compacted {
                    result: Box::new(result),
                })
            }
            Err(error) => {
                tracking.consecutive_failures = tracking.consecutive_failures.saturating_add(1);
                Ok(AutoCompactOutcome::Failed {
                    message: error.to_string(),
                })
            }
        }
    }

    async fn compact(
        &self,
        request: CompactRequest,
        cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError>;
}

/// 一次主循环运行内的自动压缩状态（docs/specs/rust-cli/compact.md §2）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactTracking {
    /// 本次运行里自动压缩成功过。
    #[serde(default)]
    pub compacted: bool,
    /// 上一次成功压缩之后的工具轮数。
    #[serde(default)]
    pub turn_counter: u32,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub consecutive_rapid_refills: u32,
}

impl CompactTracking {
    /// 进入下一个工具轮：工具轮计数加 1。
    pub fn next_turn(&mut self) {
        self.turn_counter = self.turn_counter.saturating_add(1);
    }
}

/// 发请求前的自动压缩结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutoCompactOutcome {
    NotNeeded,
    /// 已压缩；调用方用 [`RunSink::commit_messages`] 提交边界、摘要与附件。
    Compacted {
        result: Box<CompactResult>,
    },
    /// 压缩失败（已计入 [`CompactTracking::consecutive_failures`]），照常发请求。
    Failed {
        message: String,
    },
    /// rapid refill 熔断：以 API 错误结束本次运行，`message` 是给用户的错误文字。
    Thrashing {
        message: String,
    },
}

/// 压缩产生的清单增量（延迟工具、agent、MCP instructions，按保留段算增量）。由 context 实现、runtime
/// 组装；compact 把结果排在重读的文件之后，与边界、摘要一个事务提交（docs/specs/rust-cli/compact.md §3.4）。
/// 之后每一步的宣布看到它们已在边界之后，不再重复。
#[async_trait]
pub trait PostCompactContext: Send + Sync {
    /// `kept` 是保留尾段（没有时为空）。
    async fn post_compact(
        &self,
        request: ContextRequest,
        kept: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactResult {
    pub boundary: CompactBoundary,
    pub summary: UserMessage,
    /// 压缩后重新注入的内容（最近读过的文件、计划、已调用的 skill 等）。
    pub restored: Vec<Message>,
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
