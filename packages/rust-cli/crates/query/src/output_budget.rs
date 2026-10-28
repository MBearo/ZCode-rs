//! 目录模型请求的输出预算（docs/specs/rust-cli/query.md §1.11、model.md §9，修复线 F108，用户决定 2026-10-28）。
//!
//! 修 bug（F108）：以前主循环、子代理与压缩摘要的请求不给 Provider 补丁输出上限，补丁照配置写（例如 384,000），
//! 不看输入；输入 671,008 时请求的总量超过窗口，端点回 400。现在按窗口余量算出上限交给补丁（补丁以它与配置的上限的
//! 较小值为输入，写出的就是它）。请求体自己的 `max_tokens` 不动：显式覆盖的端点没有补丁，卡片模型的请求逐字节不变。

use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::provider::ModelCapabilities;

/// 窗口余量里给估算误差留的 token。
const MARGIN: u64 = 1_000;
/// 配置没给输出上限时的基线。
const DEFAULT_BASELINE: u64 = 32_000;
/// 摘要请求的输出上限。
const SUMMARY_LIMIT: u64 = 20_000;

/// 交给补丁的输出上限（`RequestOptions.provider_max_output_tokens`）：`min(基线, 窗口 − 估算 − 1000)`，余量不是正数或窗口
/// 未知（0）时是基线。基线是配置的上限（没有时 32000）与 `ZCODE_MAX_OUTPUT_TOKENS`（`env`）的较小值，摘要请求再与 20000
/// 取小。模型端口答不出能力时不给。`estimate` 是这一步活动历史的上下文 token（kernel-api `context_tokens`）。
pub(crate) fn provider_output_limit(
    capabilities: Option<&ModelCapabilities>,
    env: Option<u32>,
    source: &QuerySource,
    estimate: u64,
) -> Option<u32> {
    let capabilities = capabilities?;
    let mut baseline = capabilities.max_output_tokens.unwrap_or(DEFAULT_BASELINE);
    if let Some(env) = env {
        baseline = baseline.min(u64::from(env));
    }
    if *source == QuerySource::Compact {
        baseline = baseline.min(SUMMARY_LIMIT);
    }
    let window = capabilities.context_window;
    let available = window.saturating_sub(estimate.saturating_add(MARGIN));
    let limit = if window > 0 && available > 0 {
        baseline.min(available)
    } else {
        baseline
    };
    Some(u32::try_from(limit).unwrap_or(u32::MAX))
}
