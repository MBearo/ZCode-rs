use serde::Serialize;
use std::fmt;

/// 分类字段可跨 adapter/app 边界；诊断文案仅使用受控常量，避免泄漏供应商响应。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFailure {
    pub code: &'static str,
    pub reason: &'static str,
    pub message: &'static str,
    pub retryable: bool,
    pub status_code: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub output_committed: bool,
    #[serde(skip)]
    pub empty_completion: bool,
}
impl ModelFailure {
    pub fn new(reason: &'static str, retryable: bool) -> Self {
        let (code, message) = match reason {
            "cancelled" => ("model_request_cancelled", "Model request was cancelled."),
            "timeout" => ("model_request_timeout", "Model request timed out."),
            "stream_idle_timeout" => ("model_request_timeout", "Model stream stalled."),
            "rate_limited" => (
                "model_rate_limited",
                "Provider rate limited the model request.",
            ),
            "auth_failed" => ("provider_not_configured", "Provider authentication failed."),
            "context_exceeded" => ("model_context_exceeded", "Model context window exceeded."),
            "attachment_unavailable" => (
                "attachment_unavailable",
                "An attachment snapshot is missing or invalid.",
            ),
            "attachment_unsupported" => (
                "attachment_unsupported",
                "The selected model does not support an attachment format.",
            ),
            "model_output_limit_exceeded" => (
                "model_output_limit_exceeded",
                "The model's response exceeded the output token maximum.",
            ),
            "invalid_request" => (
                "invalid_model_request",
                "Provider rejected the model request.",
            ),
            "invalid_response" => (
                "invalid_model_response",
                "Provider response was invalid or incomplete.",
            ),
            "tls_error" => ("model_request_failed", "Provider TLS validation failed."),
            "network_error" => (
                "model_request_failed",
                "Provider connection or stream interrupted.",
            ),
            "provider_overloaded" => ("model_request_failed", "Provider is overloaded."),
            "server_error" => ("model_request_failed", "Provider returned a server error."),
            _ => ("model_request_failed", "Model request failed."),
        };
        Self {
            code,
            reason,
            message,
            retryable,
            status_code: None,
            retry_after_ms: None,
            output_committed: false,
            empty_completion: false,
        }
    }
    pub fn invalid() -> Self {
        Self::new("invalid_response", false)
    }
    pub fn cancelled() -> Self {
        Self::new("cancelled", false)
    }
    pub fn empty() -> Self {
        Self {
            empty_completion: true,
            ..Self::new("invalid_response", true)
        }
    }
}
impl fmt::Display for ModelFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for ModelFailure {}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryState {
    pub attempt: u32,
    pub max_attempts: u32,
    pub next_retry_at: u64,
    pub reason_code: &'static str,
}
