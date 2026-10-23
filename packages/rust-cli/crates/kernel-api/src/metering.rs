//! 用量的计量事实与查询（docs/specs/rust-cli/usage.md，工作线 L32）。
//!
//! 计时只在主循环里读注入的时钟（请求前、首个增量、流结束；工具开工与结束），作为事实与所属的消息同批提交；
//! 存储映射成 Node 的 `model_usage` / `tool_usage` / `turn_usage`。`/usage` 的 API 时长与活动统计都从存储查，
//! 不在内存里另记累计值。

use serde::{Deserialize, Serialize};

use crate::context_usage::format_tokens;
use crate::ids::{MessageId, ToolUseId};
use crate::message::{StopReason, Usage};
use crate::model::{ModelError, ModelSelection, QuerySource};

/// 一次调用或一个回合的结局（Node 用量表的 `status`；`running` 不写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Completed,
    Error,
    /// 用户中断。
    Cancelled,
}

/// 一次模型请求（一次 `ModelPort` 调用，适配器内的重试算在里面）：Node `model_usage` 的一行（usage.md §2.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCall {
    /// 这次响应的 assistant 消息 id（请求前生成）。没有提交消息时也用它作逻辑请求 id。
    pub message: MessageId,
    pub source: QuerySource,
    /// 发请求时的选择快照：运行中切换模型不影响这一行。
    pub selection: ModelSelection,
    pub started_at_ms: u64,
    /// 第一个非空的文本或推理增量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_at_ms: Option<u64>,
    pub completed_at_ms: u64,
    pub status: CallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// 响应里的 tool_use 块数。
    #[serde(default)]
    pub tool_calls: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 适配器内部重试的次数（`ModelNotice::Retrying`）。
    #[serde(default)]
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ModelError>,
}

/// 一次工具调用：Node `tool_usage` 的一行（usage.md §2.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_use: ToolUseId,
    pub name: String,
    /// 执行器开工的时刻；没有开工就有结果时等于 `completed_at_ms`。
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub status: CallStatus,
    /// 结果里文字的 UTF-8 字节数。
    #[serde(default)]
    pub output_bytes: u64,
}

/// [`crate::orchestration::RunSink::commit_metered`] 的计量部分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "call", rename_all = "snake_case")]
pub enum Metered {
    Model(ModelCall),
    Tool(ToolCall),
}

/// `/usage` 的会话部分里来自存储的值（usage.md §4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetering {
    /// 会话及其子代理子会话里成功请求的耗时之和（`/usage` 的 `Total duration (API)`）。
    pub api_duration_ms: u64,
}

/// footer 的请求统计（docs/specs/rust-cli/tui-display.md §2.6、usage.md §4.1，修复线 F27）：本会话完成的主回合请求
/// （`query_source = main_turn`、`status = completed`）在用量表里的聚合。存储是唯一来源：前端每次重查，不在内存里
/// 累加；显示的值由 [`Self::tokens_per_second`] 与 [`Self::cache_hit_rate`] 按 Node 的口径算。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestStats {
    /// 完成的主回合请求数（Node `hitRateRequestCount`）。
    pub requests: u64,
    /// 累计输入：Node 归一化后的 `inputTokens`（AI SDK v6 的总输入，含缓存读与缓存写）。
    pub input_tokens: u64,
    /// 累计缓存读。
    pub cache_read_tokens: u64,
    /// 有一次请求的输入或缓存读未知（Node `hasUnknownCacheUsage`）。
    #[serde(default)]
    pub unknown_cache_usage: bool,
    /// 最近一个完成的主回合请求（按完成时刻）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<LastRequest>,
}

/// 最近一个完成的主回合请求：TPS 的输入（用量表那一行的 `output_tokens`、`duration_ms`、`time_to_first_token_ms`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRequest {
    /// 输出 token；用量未知时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// 请求总时长。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// 首个输出内容的时间（相对请求开始；第一个非空的文本或推理增量，Node `firstModelTokenAt`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
}

impl LastRequest {
    /// 生成时长：总时长大于首字时间时是两者之差，否则没有（Node 调试面板的 `generationDurationMs`）。
    pub fn generation_ms(&self) -> Option<u64> {
        let (duration, first) = (self.duration_ms?, self.first_token_ms?);
        (duration > first).then(|| duration - first)
    }
}

impl RequestStats {
    /// 最近一个完成的主回合请求的输出速度（[`output_tps`]）。
    pub fn tokens_per_second(&self) -> Option<f64> {
        let last = self.last?;
        output_tps(last.output_tokens, last.generation_ms())
    }

    /// 累计缓存命中率 `缓存读 ÷ 输入`：有一次未知或累计输入为 0 时没有值（Node 调试面板的 `cache.hitRate`）。
    #[allow(clippy::cast_precision_loss)]
    pub fn cache_hit_rate(&self) -> Option<f64> {
        (!self.unknown_cache_usage && self.input_tokens > 0)
            .then(|| self.cache_read_tokens as f64 / self.input_tokens as f64)
    }
}

/// Node `calculateOutputTps`（`packages/shared/src/session-debug.ts`）：`output × 1000 / 生成时长`；任一未知或生成时长
/// 不大于 0 时没有值，不用请求总时长代替。
#[allow(clippy::cast_precision_loss)]
pub fn output_tps(output_tokens: Option<u64>, generation_ms: Option<u64>) -> Option<f64> {
    let output = output_tokens?;
    let generation = generation_ms.filter(|ms| *ms > 0)?;
    let tps = output as f64 * 1000.0 / generation as f64;
    tps.is_finite().then_some(tps)
}

/// 活动统计的日期范围（`all` / `7d` / `30d`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityRange {
    All,
    Week,
    Month,
}

impl ActivityRange {
    /// 范围包含的天数；`Self::All` 没有上限。
    pub fn days(self) -> Option<u64> {
        match self {
            Self::All => None,
            Self::Week => Some(7),
            Self::Month => Some(30),
        }
    }

    /// 切换顺序：All time → Last 7 days → Last 30 days → All time。
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Week,
            Self::Week => Self::Month,
            Self::Month => Self::All,
        }
    }
}

/// 存储的活动统计查询：本地日由 `utc_offset_minutes` 换算，`today` 是本地的今天（`YYYY-MM-DD`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityQuery {
    pub range: ActivityRange,
    pub today: String,
    pub utc_offset_minutes: i32,
}

/// 一个本地日的消息数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityDay {
    pub date: String,
    pub messages: u64,
}

/// 一个模型的 token（`input` 不含缓存读写）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTokens {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl ModelTokens {
    /// 四项之和。
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

/// 活动统计的原始聚合（usage.md §5.2）；连续天数等由前端从它算。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityStats {
    pub range: Option<ActivityRange>,
    /// 查询时本地的今天。
    pub today: String,
    /// 有消息的本地日，按日期升序。
    pub days: Vec<ActivityDay>,
    /// 按 token 总数降序。
    pub models: Vec<ModelTokens>,
    /// 范围内开始的会话数。
    pub sessions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longest_session_ms: Option<u64>,
    /// 范围内最早与最晚的会话开始（Unix 毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_session_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_ms: Option<u64>,
}

/// 时长的写法：不到一分钟是秒数加 `s`；否则 `Xm Ys`、`Xh Ym Zs`、`Xd Yh Zm`。
pub fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }
    let (mut days, mut hours, mut minutes) = (
        ms / 86_400_000,
        (ms % 86_400_000) / 3_600_000,
        (ms % 3_600_000) / 60_000,
    );
    // 按 Math.round 取秒；毫秒余数小于 60000，四舍五入后最多 60。
    let mut seconds = (ms % 60_000 + 500) / 1000;
    if seconds == 60 {
        seconds = 0;
        minutes += 1;
    }
    if minutes == 60 {
        minutes = 0;
        hours += 1;
    }
    if hours == 24 {
        hours = 0;
        days += 1;
    }
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

/// 同 [`format_duration`]，末尾为 0 的单位不写（`10m`、`1h 30m`、`2h`、`1d`；工作线 F69 新增：超时转后台的说明里的
/// 期限，docs/specs/rust-cli/shell-permissions.md §1.2.1）。不到一分钟时与 [`format_duration`] 相同。
pub fn format_duration_trimmed(ms: u64) -> String {
    let full = format_duration(ms);
    if ms < 60_000 {
        return full;
    }
    let mut units: Vec<&str> = full.split(' ').collect();
    // 从最后一个单位往前去掉值为 0 的，至少留一个。
    while units.len() > 1
        && units
            .last()
            .is_some_and(|unit| unit.trim_end_matches(char::is_alphabetic) == "0")
    {
        units.pop();
    }
    units.join(" ")
}

/// 数字的写法：小于 1000 原样；否则 compact 写法、固定一位小数（`1.0k`）。
pub fn format_number(value: u64) -> String {
    let text = format_tokens(value);
    if value < 1000 || text.contains('.') {
        return text;
    }
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    format!("{}.0{}", &text[..split], &text[split..])
}

/// `/usage` 会话部分的输入（usage.md §4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSummary {
    pub api_duration_ms: u64,
    pub wall_duration_ms: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
    /// 按模型累计的用量，按首次出现的顺序。
    pub models: Vec<(String, Usage)>,
}

impl SessionSummary {
    /// 会话用量摘要的文字，去掉没有数据来源的费用（usage.md D1）与缓存行（D9）。
    pub fn text(&self) -> String {
        let lines = |n: u64| if n == 1 { "line" } else { "lines" };
        let mut out = format!(
            "Total duration (API):  {}\nTotal duration (wall): {}\nTotal code changes:    {} {} added, {} {} removed\n",
            format_duration(self.api_duration_ms),
            format_duration(self.wall_duration_ms),
            self.lines_added,
            lines(self.lines_added),
            self.lines_removed,
            lines(self.lines_removed),
        );
        if self.models.is_empty() {
            out.push_str("Usage:                 0 input, 0 output, 0 cache read, 0 cache write");
            return out;
        }
        out.push_str("Usage by model:");
        for (model, usage) in &self.models {
            out.push_str(&format!(
                "\n{:>21}  {} input, {} output, {} cache read, {} cache write",
                format!("{model}:"),
                format_number(usage.input_tokens),
                format_number(usage.output_tokens),
                format_number(usage.cache_read_input_tokens),
                format_number(usage.cache_creation_input_tokens),
            ));
        }
        out
    }
}

#[cfg(test)]
#[path = "metering_tests.rs"]
mod tests;
