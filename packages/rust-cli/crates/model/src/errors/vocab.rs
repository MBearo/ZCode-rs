//! 各协议与厂商共用的错误词（docs/specs/rust-cli/model.md §5.1.1，provider-errors.md §5.1）：上下文超长（修复线 F108），
//! 配额、内容拦截、太大、限流、过载的通用码（修复线 F113）。Anthropic 与 OpenAI 两个分类器共用；某个厂商专有的码不在这里
//! （智谱的在 `zhipu.rs`）。
//!
//! 修 bug（F108）：以前 Anthropic 一侧只认 `prompt is too long`，兼容端点用 OpenAI 的句式（`This model's maximum context
//! length is … tokens. However, you requested …`）报超长时归成 `InvalidRequest`，不触发反应式压缩，回合直接失败；OpenAI
//! 一侧另有自己的四项。现在两侧共用这一份，再补上各家常见的说法。

/// 错误码（出现在正文里就算：`error.code`，或消息正好是这个码）。
const CODES: [&str; 4] = [
    "context_length_exceeded",
    "context_window_exceeded",
    "model_context_exceeded",
    "model_context_window_exceeded",
];

/// 单独出现就算的说法。
const PHRASES: [&str; 4] = [
    "prompt is too long",
    "maximum context length",
    "context window",
    "range of input length should be",
];

/// 要同时出现的几组词（各家的说法里没有上面的短语）。
///
/// 第三组是“输入加输出上限超出窗口”（``input length and `max_tokens` exceed context limit: …``）：F113 起也按上下文超限
/// 压缩（provider-errors.md D8 暂定 (b)），不做调小 `max_tokens` 的重发。以前它是 `InvalidRequest`，回合直接失败。
const TOGETHER: [&[&str]; 3] = [
    &[
        "input token count",
        "exceed",
        "maximum number of tokens allowed",
    ],
    &["total message token length", "exceed", "model limit"],
    &["input length and", "max_tokens", "exceed context limit"],
];

/// 正文（不区分大小写）说的是不是上下文超长。
pub(super) fn context_exceeded(raw: &str) -> bool {
    let raw = raw.to_lowercase();
    CODES
        .iter()
        .chain(PHRASES.iter())
        .any(|needle| raw.contains(needle))
        || TOGETHER
            .iter()
            .any(|words| words.iter().all(|word| raw.contains(word)))
}

/// 余额、配额、额度用尽的码（`error.type` 或 `error.code`；provider-errors.md §5.1，D4）：不重试。
pub(super) const QUOTA_CODES: [&str; 7] = [
    "insufficient_quota",
    "billing_error",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
    "exceeded_current_quota_error",
];

/// 内容安全拦截的错误码（不是结束原因；provider-errors.md §2.3）。
pub(super) const CONTENT_FILTER_CODES: [&str; 1] = ["content_filter"];

/// 请求体太大的错误类型（Anthropic 的 413，流里的错误按类型来）。
pub(super) const TOO_LARGE_CODES: [&str; 1] = ["request_too_large"];

/// 限流的码。
pub(super) const RATE_LIMIT_CODES: [&str; 3] = [
    "rate_limit_error",
    "rate_limit_exceeded",
    "rate_limit_reached_error",
];

/// 过载的码。
pub(super) const OVERLOADED_CODES: [&str; 3] = [
    "overloaded_error",
    "server_is_overloaded",
    "engine_overloaded_error",
];

/// 媒体太大的说法（图片过大、尺寸过大；provider-errors.md §5.2，D9 暂定）。只在 4xx 上看。
const MEDIA_TOO_LARGE: [&str; 2] = ["image exceeds", "image dimensions exceed"];

/// 正文（不区分大小写）说的是不是媒体太大。
pub(super) fn media_too_large(raw: &str) -> bool {
    let raw = raw.to_lowercase();
    MEDIA_TOO_LARGE.iter().any(|needle| raw.contains(needle))
}
