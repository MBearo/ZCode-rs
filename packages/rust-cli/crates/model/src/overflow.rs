//! 上下文超长的词表（docs/specs/rust-cli/model.md §5.1.1，修复线 F108）：Anthropic 与 OpenAI 两个分类器共用。
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
const TOGETHER: [&[&str]; 2] = [
    &[
        "input token count",
        "exceed",
        "maximum number of tokens allowed",
    ],
    &["total message token length", "exceed", "model limit"],
];

/// 正文（不区分大小写）说的是不是上下文超长。
pub(crate) fn context_exceeded(raw: &str) -> bool {
    let raw = raw.to_lowercase();
    CODES
        .iter()
        .chain(PHRASES.iter())
        .any(|needle| raw.contains(needle))
        || TOGETHER
            .iter()
            .any(|words| words.iter().all(|word| raw.contains(word)))
}
