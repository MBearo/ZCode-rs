//! 窗口、阈值与容量级别（docs/specs/rust-cli/compact.md §2）。
//!
//! 有效窗口、自动压缩的阈值、容量级别与窗口的来源。

use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::TokenLimits;
use rust_cli_prompts::models;

/// 摘要输出的预留上限。
const SUMMARY_OUTPUT_RESERVE: u64 = 20_000;
/// 阈值离有效窗口的距离。
const AUTOCOMPACT_BUFFER: u64 = 13_000;
/// 告警线离阈值的距离。
const WARNING_BUFFER: u64 = 20_000;
/// 阻塞线离窗口的距离。
const BLOCKING_BUFFER: u64 = 3_000;
/// 原生 1M 窗口。
const NATIVE_1M: u64 = 1_000_000;
/// 窗口覆盖的上下限。
const OVERRIDE_MIN: u64 = 100_000;
const OVERRIDE_MAX: u64 = 1_000_000;
/// 百分比的满值（compact 的产品代码不依赖 base，不能引用 `units::PERCENT_F64`）。
const PERCENT: f64 = 100.0;

/// 窗口从哪里来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSource {
    /// 配置的窗口覆盖（`ZCODE_AUTO_COMPACT_WINDOW`）。
    Override,
    /// 卡片认识、窗口 ≥ 1M 的模型。
    ModelDefault,
    /// 卡片不认识的模型。
    UnknownModel,
    /// 卡片认识、窗口 < 1M：本地交给反应式压缩，不主动压缩。
    Auto,
}

/// 计算阈值要用的模型事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFacts {
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub recognized: bool,
}

impl ModelFacts {
    /// 窗口与输出上限只取模型上限查询的结果 `limits`（生效配置，compact.md §2、model.md §11）；答不出时（配置里没有
    /// 这个模型、测试）按卡片回退，都没有时用默认窗口。查询没给输出上限时按卡片，再没有时不限。`recognized`（认不认识
    /// 这个模型，决定窗口来源）只看卡片——它只对卡片里的模型有意义。
    ///
    /// 修复线 F23（用户决定 A5）：以前卡片优先，卡片里的窗口盖过 Provider 配置。修复线 F61：以前参数是目录，只有
    /// `auto_compact` 传了它，三个同步方法与显式覆盖都按卡片或默认窗口。
    pub fn resolve(selection: &ModelSelection, limits: Option<TokenLimits>) -> Self {
        let card = models::card(&selection.model);
        let card_output = card.map(|card| u64::from(card.max_output_tokens));
        let (context_window, max_output_tokens) = match limits {
            Some(limits) => (
                limits.context_window,
                limits.max_output_tokens.or(card_output),
            ),
            // 不认识的模型、目录也没给出窗口时是默认窗口（200,000）：与 ToolSearch 的自动门槛同一个回退（工作线 F89，
            // 定义只在 prompts 一处）。
            None => (models::context_window(&selection.model, None), card_output),
        };
        Self {
            context_window,
            max_output_tokens: max_output_tokens.unwrap_or(u64::from(u32::MAX)),
            recognized: card.is_some(),
        }
    }
}

/// 阈值的配置（入口从设置与环境变量取值）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Thresholds {
    /// `ZCODE_AUTOCOMPACT_PCT_OVERRIDE`：(0, 100] 之外的值无效。
    pub pct_override: Option<f64>,
    /// `ZCODE_AUTO_COMPACT_WINDOW`。
    pub window_override: Option<u64>,
    /// `ZCODE_MAX_OUTPUT_TOKENS`（修复线 F42）：模型的输出上限先与它取小，再算摘要的
    /// 预留（模型的输出上限已含这个变量）。
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub window: u64,
    pub source: WindowSource,
}

pub fn window(facts: &ModelFacts, thresholds: &Thresholds) -> Window {
    if let Some(configured) = thresholds.window_override.filter(|value| *value > 0) {
        let effective = configured.clamp(OVERRIDE_MIN, OVERRIDE_MAX);
        return Window {
            window: facts.context_window.min(effective),
            source: WindowSource::Override,
        };
    }
    let source = match (facts.recognized, facts.context_window >= NATIVE_1M) {
        (true, true) => WindowSource::ModelDefault,
        (true, false) => WindowSource::Auto,
        (false, _) => WindowSource::UnknownModel,
    };
    Window {
        window: facts.context_window,
        source,
    }
}

/// 摘要输出的预留（模型的输出上限与预留上限取小）：设了 `ZCODE_MAX_OUTPUT_TOKENS` 时输出上限先取小（F42）。
fn reserve(facts: &ModelFacts, thresholds: &Thresholds) -> u64 {
    let max_output = thresholds
        .max_output_tokens
        .map_or(facts.max_output_tokens, |limit| {
            limit.min(facts.max_output_tokens)
        });
    max_output.min(SUMMARY_OUTPUT_RESERVE)
}

/// 有效窗口：窗口减去摘要输出的预留。
pub fn effective(facts: &ModelFacts, thresholds: &Thresholds) -> u64 {
    window(facts, thresholds)
        .window
        .saturating_sub(reserve(facts, thresholds))
}

/// 自动压缩的阈值。
pub fn threshold(facts: &ModelFacts, thresholds: &Thresholds) -> u64 {
    let effective = effective(facts, thresholds);
    let buffered = effective.saturating_sub(AUTOCOMPACT_BUFFER);
    match thresholds
        .pct_override
        .filter(|pct| pct.is_finite() && *pct > 0.0 && *pct <= PERCENT)
    {
        // 向下取整：`Math.floor(effective * (pct / 100))`。
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        Some(pct) => buffered.min((effective as f64 * (pct / PERCENT)).floor() as u64),
        None => buffered,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Compact,
    Blocked,
}

/// 阻塞线：模型的上下文窗口（不看窗口覆盖）减去摘要输出的预留，再减 [`BLOCKING_BUFFER`]。
fn blocking_line(facts: &ModelFacts, thresholds: &Thresholds) -> u64 {
    facts
        .context_window
        .saturating_sub(reserve(facts, thresholds))
        .saturating_sub(BLOCKING_BUFFER)
}

/// 发请求前到了阻塞线、这一步不发请求（compact.md §2.2）。自动压缩打开（`auto`）且窗口来源是 `auto` 时交给上下文超限
/// 之后的压缩，不拦。
pub fn blocks(tokens: u64, facts: &ModelFacts, thresholds: &Thresholds, auto: bool) -> bool {
    if auto && window(facts, thresholds).source == WindowSource::Auto {
        return false;
    }
    tokens >= blocking_line(facts, thresholds)
}

/// 容量级别（自动压缩打开时）。
pub fn level(tokens: u64, facts: &ModelFacts, thresholds: &Thresholds) -> Level {
    let threshold = threshold(facts, thresholds);
    let blocking = blocking_line(facts, thresholds);
    if tokens >= blocking {
        Level::Blocked
    } else if tokens >= threshold {
        Level::Compact
    } else if tokens >= threshold.saturating_sub(WARNING_BUFFER) {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// 输入框下方“上下文快满”的提示（compact.md §2.1）：先算级别，再选文字。`auto` 是
/// 自动压缩开关；`compact_command` 是 `/compact` 能不能用（`DISABLE_COMPACT` 没设时接“Run /compact”）。
///
/// 缺陷修复（F14）：以前 TUI 按 `(token, 有效窗口)` 自己算，只有 `N% context used` 一种文字，不看窗口来源与自动
/// 压缩开关，也不看百分比覆盖。
pub fn notice(
    tokens: u64,
    facts: &ModelFacts,
    thresholds: &Thresholds,
    auto: bool,
    compact_command: bool,
) -> ContextNotice {
    let effective = effective(facts, thresholds);
    let threshold = threshold(facts, thresholds);
    // 级别的基准：自动压缩打开时是阈值、关闭时是有效窗口；阻塞线按模型的上下文窗口（不看窗口覆盖）。
    let base = if auto { threshold } else { effective };
    let blocking = blocking_line(facts, thresholds);
    let level_ok = tokens < blocking
        && !(auto && tokens >= threshold)
        && tokens < base.saturating_sub(WARNING_BUFFER);
    if level_ok {
        return ContextNotice::Hidden;
    }
    let left = percent_left(base, tokens);
    if !auto {
        return ContextNotice::Low {
            percent: left,
            compact_hint: compact_command,
        };
    }
    if window(facts, thresholds).source == WindowSource::Auto {
        // 与 footer 的 ctx 同一个式子（kernel-api `used_percent`，F27）。
        ContextNotice::Used {
            percent: rust_cli_kernel_api::compaction::used_percent(tokens, effective),
        }
    } else {
        ContextNotice::UntilAutoCompact { percent: left }
    }
}

/// `Math.max(0, Math.round((base − tokens) / base · 100))`：按 JS 的双精度算，`.5` 的边界与 JS 相同。负数最后取 0，
/// 所以 `f64::round`（远离 0）与 JS `Math.round`（向 +∞）在这里结果相同。
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn percent_left(base: u64, tokens: u64) -> u64 {
    if base == 0 {
        return 0;
    }
    let ratio = (base as f64 - tokens as f64) / base as f64 * PERCENT;
    ratio.round().max(0.0) as u64
}

/// 发请求前要不要主动压缩（本地的判断）：来源是 `auto` 时交给反应式压缩。
pub fn should_compact(tokens: u64, facts: &ModelFacts, thresholds: &Thresholds) -> bool {
    if window(facts, thresholds).source == WindowSource::Auto {
        return false;
    }
    matches!(
        level(tokens, facts, thresholds),
        Level::Compact | Level::Blocked
    )
}

/// `/context` 的缓冲类别（context-usage.md §4）：自动压缩开启且来源不是 `auto` 时
/// `Autocompact buffer = 窗口 − 阈值`（阈值不看百分比覆盖）；关闭时 `Compact buffer = 3000`；否则没有。
pub fn context_buffer(
    facts: &ModelFacts,
    thresholds: &Thresholds,
    auto: bool,
) -> Option<(&'static str, u64)> {
    let window = window(facts, thresholds);
    if !auto {
        return Some(("Compact buffer", BLOCKING_BUFFER));
    }
    if window.source == WindowSource::Auto {
        return None;
    }
    let threshold = window
        .window
        .saturating_sub(reserve(facts, thresholds))
        .saturating_sub(AUTOCOMPACT_BUFFER);
    Some(("Autocompact buffer", window.window - threshold))
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
