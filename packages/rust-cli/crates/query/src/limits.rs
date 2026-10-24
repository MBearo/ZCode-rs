//! 入口按环境变量给的请求覆盖（docs/specs/rust-cli/query.md §1.5、cli-args.md §9.1、§9.2，修复线 F42）：输出 token
//! 上限（`ZCODE_MAX_OUTPUT_TOKENS`）、thinking 开关（`MAX_THINKING_TOKENS`）与 ToolSearch 开不开（`ENABLE_TOOL_SEARCH`，
//! 工作线 F89，tools-more.md §3.2.1）。
//! 值的读法在入口（cli `limits.rs`），这里只按这一步的模型算请求参数。

use rust_cli_prompts::models::{ModelCard, ToolSearchSetting};

/// 没有卡片的模型的输出上限的缺省与上限。
pub(crate) const UNKNOWN_DEFAULT: u32 = rust_cli_prompts::models::UNKNOWN_MAX_OUTPUT_TOKENS;
const UNKNOWN_UPPER: u32 = 128_000;

/// `MAX_THINKING_TOKENS` 的三种结果（启动时的 thinking 配置）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingTokens {
    /// 没设或空：按模型（卡片的 adaptive thinking）。
    #[default]
    Unset,
    /// 大于 0：`{type: enabled, budgetTokens}`。adaptive 模型不用预算，本工程的模型都是
    /// adaptive 或不发 thinking，请求与 `Unset` 相同。
    Budget(u32),
    /// 0、负数或不是数字：thinking 关。
    Disabled,
}

/// 一次运行内不变的覆盖值（runtime 从 `Defaults.limits` 交进 [`crate::QueryParts`]）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestLimits {
    /// `ZCODE_MAX_OUTPUT_TOKENS` 的有效值（正整数）；None 是没设或无效（用缺省）。
    pub max_output_tokens: Option<u32>,
    pub thinking: ThinkingTokens,
    /// `ENABLE_TOOL_SEARCH`：主循环与子代理的每一步按它判断 ToolSearch 开不开（[`crate::step_tool_search`]）。
    pub tool_search: ToolSearchSetting,
}

impl RequestLimits {
    /// 主请求的 `max_tokens`（由值、缺省与上限算出）：有卡片时缺省与上限都是卡片的值（claude-opus-5-5 的
    /// 模型表是 128000 / 128000），没有卡片时 32000 / 128000；设了值时取它与上限的较小值。
    pub(crate) fn max_tokens(&self, card: Option<&ModelCard>) -> u32 {
        let (default, upper) = card.map_or((UNKNOWN_DEFAULT, UNKNOWN_UPPER), |card| {
            (card.max_output_tokens, card.max_output_tokens)
        });
        self.max_output_tokens
            .map_or(default, |value| value.min(upper))
    }

    /// 这一步发不发 thinking：thinking 关时有卡片的模型也不发（claude-opus-5-5 拒绝 `{type: disabled}`，直接
    /// 不写 `thinking`）。
    pub(crate) fn thinking_allowed(&self) -> bool {
        self.thinking != ThinkingTokens::Disabled
    }
}
