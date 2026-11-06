//! 回合失败的事实（docs/puffin-tui.md §4.2「回合失败」；Puffin `host/fold/turns.rs` 的 `failure`）：原因、HTTP 状态、能不能
//! 再试、试过几次、哪个 provider 与模型、可以再发的那条消息、失败的时刻。块的措辞与画法在 kit 的 `failure_block`，按原因
//! 写，从不看消息的文字（`ui/cells/failure.rs`）。以前失败的回合是一行红色的原文。
use rust_cli_kernel_api::compaction::PROMPT_TOO_LONG;
use rust_cli_kernel_api::model::ModelError;

use super::{Body, Transcript};

/// 一次回合失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    /// 认得的原因（`rate_limited`、`auth_failed`…）；不知道时为 ""。
    pub reason: &'static str,
    pub http_status: u16,
    pub retryable: bool,
    /// 这个回合里重试过时是试过的次数，否则 0。
    pub tries: u32,
    pub provider: String,
    pub model: String,
    /// 这个回合的第一条你的消息（⏎ 再发它）；没有时为 ""。
    pub resend: String,
    /// 失败的时刻（epoch ms）。
    pub at: u64,
}

/// 失败的原因、HTTP 状态与能不能再试，按 `ModelError`（Puffin `reason_of_error`）。
fn reason_of_error(error: &ModelError) -> (&'static str, u16, bool) {
    match error {
        ModelError::RateLimited { .. } => ("rate_limited", 429, true),
        ModelError::Overloaded { .. } => ("provider_overloaded", 529, true),
        ModelError::PromptTooLong { .. } => ("context_exceeded", 0, false),
        ModelError::Authentication { .. } => ("auth_failed", 401, false),
        ModelError::InvalidRequest { .. } => ("invalid_request", 400, false),
        ModelError::Network { .. } => ("network_failed", 0, true),
        ModelError::Server { status, .. } => ("server_error", *status, true),
        ModelError::StreamIdle { .. } => ("stream_idle_timeout", 0, true),
        _ => ("", 0, false),
    }
}

/// 失败的原因，按失败那句话里 `ModelError` 的说法（Puffin `reason_of_message`）。
fn reason_of_message(message: &str) -> Option<(&'static str, u16, bool)> {
    let lower = message.to_lowercase();
    if message.starts_with(PROMPT_TOO_LONG) || lower.contains("prompt too long") {
        return Some(("context_exceeded", 0, false));
    }
    if let Some(at) = lower.find("server error ") {
        let status = lower[at + "server error ".len()..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(0);
        return Some(("server_error", status, true));
    }
    [
        ("rate limited:", ("rate_limited", 429, true)),
        ("model overloaded:", ("provider_overloaded", 529, true)),
        ("authentication failed:", ("auth_failed", 401, false)),
        ("invalid request:", ("invalid_request", 400, false)),
        ("network error:", ("network_failed", 0, true)),
        ("stream idle timeout", ("stream_idle_timeout", 0, true)),
    ]
    .into_iter()
    .find(|(words, _)| lower.contains(words))
    .map(|(_, reason)| reason)
}

impl Transcript {
    /// 回合失败了：失败的块（Puffin `failure`）。
    pub(super) fn failed(&mut self, message: String, now_ms: u64) {
        let retried = self.retried.take();
        let (reason, http_status, retryable) = reason_of_message(&message)
            .or_else(|| retried.as_ref().map(|(_, error)| reason_of_error(error)))
            .unwrap_or(("", 0, false));
        let selection = self.selection().clone();
        let failure = Failure {
            message,
            reason,
            http_status,
            retryable,
            tries: retried.map_or(0, |(attempt, _)| attempt + 1),
            provider: selection.provider,
            model: selection.model,
            resend: self.turn_user.clone().unwrap_or_default(),
            at: now_ms,
        };
        self.push(Body::Failure(Box::new(failure)), now_ms);
    }

    /// 一次重试：记下这个回合里最多的一次与它的错误（失败的块写试过几次）。
    pub(super) fn retrying(&mut self, attempt: u32, error: &ModelError) {
        let most = self
            .retried
            .as_ref()
            .map_or(attempt, |(seen, _)| (*seen).max(attempt));
        self.retried = Some((most, error.clone()));
    }
}
