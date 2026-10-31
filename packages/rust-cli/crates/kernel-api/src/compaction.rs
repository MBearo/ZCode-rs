//! 压缩的编排接口（docs/specs/rust-cli/compact.md）：请求、结果、自动压缩的运行状态与 [`Compactor`]。
//! 由 [`crate::orchestration`] 重新导出。

use async_trait::async_trait;
use rust_cli_base::tokens;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::env::WorkspaceRef;
use crate::ids::{AgentId, SessionId};
use crate::message::{
    AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, ImageSource, Message,
    SystemMessage, ToolResultContent, Usage, UserMessage,
};
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

/// 上下文超限错误文本里的 token 差，认两种句式（docs/specs/rust-cli/compact.md §3.1）：
///
/// - `prompt is too long … N tokens > M`（按正则 `/prompt is too long[^0-9]*(\d+)\s*tokens?\s*>\s*(\d+)/i` 取两个数），
///   差是 N − M。例：`prompt is too long: 1000500 tokens > 1000000 maximum` → 500。
/// - `maximum context length is N tokens … (A in the messages, …)`（OpenAI 的句式，兼容端点也回它，修复线 F108），
///   差是输入 A − 上限 N。
///
/// 差不大于 0 时为 None（OpenAI 句式里输入没超、是加上输出上限才超的情形）。
pub fn prompt_too_long_gap(message: &str) -> Option<u64> {
    anthropic_gap(message).or_else(|| openai_gap(message))
}

/// `prompt is too long … N tokens > M`：N − M。
fn anthropic_gap(message: &str) -> Option<u64> {
    const PHRASE: &str = "prompt is too long";
    // 数字后面的单位词（之后可以有 `s`）。
    const TOKEN_WORD: &str = "token";
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
    let rest = match rest.get(..TOKEN_WORD.len()) {
        Some(word) if word.eq_ignore_ascii_case(TOKEN_WORD) => &rest[TOKEN_WORD.len()..],
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

/// `maximum context length is N tokens … (A in the messages, …)`：A − N。修 bug（F108）：以前只认上面那种句式，兼容端点回
/// 这一句时没有差，压缩摘要请求再超长时裁不出该裁的量。
fn openai_gap(message: &str) -> Option<u64> {
    const LIMIT: &str = "maximum context length is";
    const INPUT: &str = " in the messages";
    let lower = message.to_ascii_lowercase();
    let rest = lower[lower.find(LIMIT)? + LIMIT.len()..].trim_start();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let limit: u64 = rest[..digits].parse().ok()?;
    let rest = &rest[digits..];
    if !rest.trim_start().starts_with("token") {
        return None;
    }
    // 输入是 ` in the messages` 前面紧挨着的那个数（只有 ASCII 数字，切在字节上不会落在字符中间）。
    let before = &rest[..rest.find(INPUT)?];
    let start = before.len() - before.bytes().rev().take_while(u8::is_ascii_digit).count();
    let input: u64 = before[start..].parse().ok()?;
    input.checked_sub(limit).filter(|gap| *gap > 0)
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
    // 百分比的满值。
    const FULL: u64 = rust_cli_base::units::PERCENT;
    if window == 0 {
        return FULL;
    }
    let left = (window as f64 - used_tokens as f64) / window as f64 * FULL as f64;
    FULL - (left.round().max(0.0) as u64).min(FULL)
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

    /// 发请求前的阻塞线（docs/specs/rust-cli/compact.md §2.2）：估算的 token 到了阻塞线、又不交给反应式压缩时，返回
    /// 结束这次运行的错误文字，这一步不发请求。`failure` 是这一步自动压缩失败的说明（[`AutoCompactOutcome::Failed`]
    /// 的 `message`），没有失败时为 None。默认不拦。
    fn blocking_limit(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
        _failure: Option<&str>,
    ) -> Option<String> {
        None
    }
}

/// 文字的粗估 token：字符数（UTF-16 码元，JS 的 `length`）/ 4，四舍五入。压缩的预算与压缩后附件的上限都用它。
pub fn rough_tokens(text: &str) -> u64 {
    let units = text.encode_utf16().count() as u64;
    // 每 token 的字符数与 Math.round 只定义在 base 的 `tokens`。
    tokens::estimate(units, tokens::CHARS_PER_TOKEN)
}

/// 图片与文档的估算 token（压缩的估算、`/context` 与目录模型的输出预算共用）。
pub const IMAGE_TOKENS: u64 = 2_000;

fn result_tokens(content: &[ToolResultContent]) -> u64 {
    content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text { text } => rough_tokens(text),
            ToolResultContent::Image { .. } | ToolResultContent::Document { .. } => IMAGE_TOKENS,
            ToolResultContent::ToolReference { tool_name } => rough_tokens(tool_name),
        })
        .sum()
}

fn block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => rough_tokens(text),
        ContentBlock::Image {
            source: ImageSource::Base64 { .. } | ImageSource::Artifact { .. },
        } => IMAGE_TOKENS,
        ContentBlock::Thinking { thinking, .. } => rough_tokens(thinking),
        ContentBlock::RedactedThinking { data } => rough_tokens(data),
        ContentBlock::ToolUse { name, input, .. } => rough_tokens(&format!("{name}{input}")),
        ContentBlock::ToolResult { content, .. } => result_tokens(content),
        ContentBlock::ServerToolUse { name, input, .. } => rough_tokens(&format!("{name}{input}")),
        ContentBlock::WebSearchToolResult { content, .. } => rough_tokens(&content.to_string()),
    }
}

/// 一条消息的粗估 token（docs/specs/rust-cli/compact.md §2）：文字按 [`rough_tokens`]，图片与文档按 [`IMAGE_TOKENS`]，
/// 工具调用按名字加输入的 JSON；attachment 的内容块（`@` 提及的图片等）与消息里的块同样估算；system 消息不算。
pub fn message_tokens(message: &Message) -> u64 {
    match message {
        Message::User(user) => user.content.iter().map(block_tokens).sum(),
        Message::Assistant(assistant) => assistant.content.iter().map(block_tokens).sum(),
        Message::Attachment(attachment) => {
            rough_tokens(&attachment.text)
                + attachment.blocks().iter().map(block_tokens).sum::<u64>()
        }
        Message::System(_) => 0,
    }
}

/// 一次响应的上下文大小（输入 + 缓存读写 + 输出）。
fn usage_total(usage: &Usage) -> u64 {
    usage.input_tokens
        + usage.cache_creation_input_tokens
        + usage.cache_read_input_tokens
        + usage.output_tokens
}

/// 活动历史的上下文 token（docs/specs/rust-cli/compact.md §2）：最近一次带用量的响应（输入 + 缓存读写 + 输出）加之后
/// 消息的粗估；遇到压缩边界时锚点是 0；都没有时是全部消息的粗估。压缩的判定、footer 的 ctx 与目录模型请求的输出预算
/// （docs/specs/rust-cli/query.md §1.11）只用这一个算法。
pub fn context_tokens(active: &[&Message]) -> u64 {
    let after = |index: usize| -> u64 {
        active[index + 1..]
            .iter()
            .copied()
            .map(message_tokens)
            .sum()
    };
    for (index, item) in active.iter().enumerate().rev() {
        let anchor = match item {
            Message::System(SystemMessage::CompactBoundary(_)) => Some(0),
            Message::Assistant(assistant) => assistant.usage.as_ref().map(usage_total),
            _ => None,
        };
        if let Some(tokens) = anchor {
            return tokens + after(index);
        }
    }
    active.iter().copied().map(message_tokens).sum()
}

/// 到了阻塞线时结束回合的错误文字（docs/specs/rust-cli/compact.md §2.2）；压缩失败、带着原因时在它后面接
/// ` · automatic compaction failed: <原因>`。界面按它换成“上下文满了”的提示。
pub const PROMPT_TOO_LONG: &str = "Prompt is too long";

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

    /// 同 [`Self::post_compact`]，另带压缩前的全部历史 `history`：要从整段会话里取的附件（调用过的 skill，
    /// docs/specs/rust-cli/compact.md §3.4）用它。缺省忽略历史。
    async fn post_compact_with_history(
        &self,
        request: ContextRequest,
        _history: &[Message],
        kept: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.post_compact(request, kept).await
    }
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
