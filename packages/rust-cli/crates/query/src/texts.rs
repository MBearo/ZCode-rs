//! 主循环里给模型看的固定文本。
//! L2 的 prompts 数据加载就绪后迁到 `prompts/`。

use rust_cli_kernel_api::model::ModelError;
use rust_cli_prompts::reminder;

/// 中断文本（kernel-api 的定义，F55：以前各 crate 各写一份）。
pub(crate) const INTERRUPT_MESSAGE: &str = rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[0];
pub(crate) const INTERRUPT_MESSAGE_FOR_TOOL_USE: &str =
    rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[1];
pub(crate) const REJECT_MESSAGE: &str = rust_cli_kernel_api::tool::REJECT_MESSAGE;

pub(crate) fn no_such_tool(name: &str) -> String {
    format!("<tool_use_error>Error: No such tool available: {name}</tool_use_error>")
}

pub(crate) fn tool_error(message: &str) -> String {
    format!("<tool_use_error>{message}</tool_use_error>")
}

/// 请求前配对兜底（query.md §1.8）的几段文字。
pub(crate) const MISSING_TOOL_RESULT: &str = "[Tool result missing due to internal error]";
pub(crate) const ORPHANED_TOOL_RESULT: &str =
    "[Orphaned tool result removed due to conversation resume]";
pub(crate) const TOOL_USE_REMOVED: &str = "[Tool use removed]";
pub(crate) const TOOL_USE_INTERRUPTED: &str = "[Tool use interrupted]";
pub(crate) const NO_CONTENT: &str = rust_cli_kernel_api::message::NO_CONTENT;

/// 输出上限续写的提示。
pub(crate) const OUTPUT_LIMIT_NUDGE: &str = "Output token limit hit. Resume directly \u{2014} no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.";

/// 没有解析出工具调用时的重试提示。
pub(crate) const MALFORMED_TOOL_USE_NUDGE: &str =
    "The previous response failed to produce a valid tool call. Please retry the tool call now.";

pub(crate) const MALFORMED_TOOL_USE_EXHAUSTED: &str =
    "The model's tool call could not be parsed (retry also failed).";

/// 没有可见输出时的提示。
pub(crate) const THINKING_ONLY_NUDGE: &str = "[Your previous response had no visible output. Please continue and produce a user-visible response.]";

/// 输出上限续写用尽（`stop_reason: max_tokens` 的错误消息）。变量名是本工程
/// 读的 `ZCODE_MAX_OUTPUT_TOKENS`（修复线 F42，config.md §2.5）；主语写成
/// `The model's response`（修复线 F51b，用户决定 2026-10-22，query.md §1）。
pub(crate) fn output_limit_exhausted(max_tokens: u32) -> String {
    format!(
        "API Error: The model's response exceeded the {max_tokens} output token maximum. To configure this behavior, set the ZCODE_MAX_OUTPUT_TOKENS environment variable."
    )
}

/// 窗口用满（`stop_reason: model_context_window_exceeded`）续写用尽时的错误消息（docs/specs/rust-cli/provider-errors.md §6，
/// D7 暂定，修复线 F113）。
pub(crate) const CONTEXT_WINDOW_EXHAUSTED: &str =
    "API Error: The model has reached its context window limit.";

/// 工具边界并入的插话（human 来源的格式，
/// 工作中收到消息的前缀在前；产品名按替换表换成 ZCode），外面包 `<system-reminder>`。
pub(crate) fn queued_prompt(prompt: &str) -> String {
    reminder::wrap(&format!(
        "The user sent a new message while you were working:\n{prompt}\n\nThis is how ZCode surfaces messages the user sends mid-turn \u{2014} within the running turn, often alongside the next tool result, rather than as a separate conversation turn. Address the message above as you continue this turn."
    ))
}

/// 断流续写的提示：主线程与子代理。
pub(crate) const TRUNCATED_NUDGE: &str = "Your response above was cut off mid-stream. Resume directly from where it stops \u{2014} no apology, no recap. If none of it survived, answer the request from the start.";
pub(crate) const TRUNCATED_NUDGE_SUBAGENT: &str = "Your response above was cut off mid-stream and only your next message is delivered. Write the complete response again from the start \u{2014} no apology, no mention of the cut-off.";

/// 已有输出后流断开的错误消息（部分收尾）。
pub(crate) fn truncated_error(error: &ModelError) -> String {
    let what = match error {
        ModelError::Server { .. } => "Server error mid-response.",
        ModelError::Protocol { .. } => "The response stream was malformed.",
        // 流空闲看门狗中止（stall-visibility.md §2.3）。
        ModelError::StreamIdle { .. } => "The response stopped arriving.",
        _ => "Connection lost mid-response.",
    };
    format!("API Error: {what} The response above may be incomplete.")
}
