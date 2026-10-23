//! 没有 tool_use 的响应之后的恢复分支（不需要后续工具轮时的判定；规则见 docs/specs/rust-cli/query.md
//! §2、§3）。
//!
//! 这里只做判定，不做 IO：主循环按返回的 [`Outcome`] 提交消息、决定下一步。

use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, StopReason};
use rust_cli_kernel_api::model::{ModelError, QuerySource};

use crate::texts;

/// 输出上限续写的次数上限。
const OUTPUT_RECOVERY_LIMIT: u8 = 3;

/// 不做 thinking-only 提示的旁路来源。
pub(crate) const QUIET_SIDE_SOURCES: [&str; 4] = [
    "prompt_suggestion",
    "away_summary",
    "agent_summary",
    "hook_prompt",
];

/// 重试时这次响应的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fate {
    /// 提交，并留在下一次请求里。
    Keep,
    /// 提交（会话记录里有），但不进下一次请求。
    Hide,
    /// 不提交（发 tombstone 删掉它）。
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 回合照常结束。
    Completed,
    /// 发一条 meta 提示后再请求一次。
    Retry {
        fate: Fate,
        nudge: &'static str,
        source: &'static str,
    },
    /// 以 API 错误结束；响应照常提交。
    Failed { message: String },
}

/// 一次 query 内的恢复计数。
#[derive(Debug, Default)]
pub(crate) struct Guards {
    output_recovery: u8,
    malformed_retried: bool,
    thinking_nudged: bool,
}

fn has_visible_text(assistant: &AssistantMessage) -> bool {
    assistant
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { text } if !text.trim().is_empty()))
}

fn nudges_thinking_only(source: &QuerySource) -> bool {
    match source {
        QuerySource::Compact => false,
        QuerySource::Side(name) => !QUIET_SIDE_SOURCES.contains(&name.as_str()),
        QuerySource::MainThread | QuerySource::Subagent(_) => true,
    }
}

impl Guards {
    /// 工具结果之后进入下一轮：全部清零。
    pub(crate) fn next_turn(&mut self) {
        *self = Self::default();
    }

    /// Stop hook 阻塞后再请求：输出上限续写
    /// 次数与 malformed 重试清零，thinking-only 提示保留。
    pub(crate) fn after_stop_block(&mut self) {
        self.output_recovery = 0;
        self.malformed_retried = false;
    }

    /// 流在已有结束的块、没有 tool_use 时断开（续写来源 `truncated_response_recovery`，只对子代理与
    /// 非交互的主线程续写；与输出上限续写共用次数）。`assistant` 是只含结束的块的半截响应。
    pub(crate) fn truncated(
        &mut self,
        assistant: &AssistantMessage,
        source: &QuerySource,
        non_interactive: bool,
        error: &ModelError,
    ) -> Outcome {
        let nudge = match source {
            QuerySource::Subagent(_) => Some(texts::TRUNCATED_NUDGE_SUBAGENT),
            QuerySource::MainThread if non_interactive => Some(texts::TRUNCATED_NUDGE),
            _ => None,
        };
        match nudge {
            Some(nudge)
                if self.output_recovery < OUTPUT_RECOVERY_LIMIT
                    && !assistant.content.is_empty() =>
            {
                self.output_recovery += 1;
                Outcome::Retry {
                    fate: Fate::Keep,
                    nudge,
                    source: "truncated_response_recovery",
                }
            }
            _ => Outcome::Failed {
                message: texts::truncated_error(error),
            },
        }
    }

    /// `assistant` 是这次响应（内容可能为空，空内容不提交）；`max_tokens` 是这次请求的输出上限。
    pub(crate) fn decide(
        &mut self,
        assistant: &AssistantMessage,
        source: &QuerySource,
        max_tokens: u32,
    ) -> Outcome {
        match assistant.stop_reason {
            Some(StopReason::MaxTokens) => {
                if self.output_recovery < OUTPUT_RECOVERY_LIMIT {
                    self.output_recovery += 1;
                    return Outcome::Retry {
                        fate: Fate::Keep,
                        nudge: texts::OUTPUT_LIMIT_NUDGE,
                        source: "max_output_tokens_recovery",
                    };
                }
                Outcome::Failed {
                    message: texts::output_limit_exhausted(max_tokens),
                }
            }
            // 能走到这里说明没有完整的 tool_use 块。
            Some(StopReason::ToolUse) => {
                if self.malformed_retried {
                    return Outcome::Failed {
                        message: texts::MALFORMED_TOOL_USE_EXHAUSTED.to_owned(),
                    };
                }
                // malformed 重试时输出上限续写次数一起清零。
                self.output_recovery = 0;
                self.malformed_retried = true;
                Outcome::Retry {
                    fate: Fate::Discard,
                    nudge: texts::MALFORMED_TOOL_USE_NUDGE,
                    source: "malformed_tool_use_retry",
                }
            }
            Some(StopReason::EndTurn | StopReason::StopSequence)
                if !self.thinking_nudged
                    && !has_visible_text(assistant)
                    && nudges_thinking_only(source) =>
            {
                self.thinking_nudged = true;
                Outcome::Retry {
                    fate: Fate::Hide,
                    nudge: texts::THINKING_ONLY_NUDGE,
                    source: "thinking_only_retry",
                }
            }
            _ => Outcome::Completed,
        }
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
