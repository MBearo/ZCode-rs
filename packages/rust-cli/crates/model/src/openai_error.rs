//! OpenAI 两种协议的错误分类（docs/specs/rust-cli/model.md §10.4）。
//!
//! 正文是 `{"error":{"message","type","code"}}`。分类沿用 Anthropic 的表（model.md §5.1），只换识别条件：
//! 上下文超长认 `code: context_length_exceeded` 与常见的消息文本；过载认 529 与 `server_is_overloaded`。
//! 能否重试与 Anthropic 一侧相同：`x-should-retry` → 408 / 409 / 429 / 401 / 5xx。

use std::time::SystemTime;

use rust_cli_base::redact::redact_secrets;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_net::ResponseHeaders;
use rust_cli_net::retry::{retry_after, should_retry};
use serde_json::Value;

// 消息的长度上限两种协议的错误共用（F55）。
use crate::protocol::{ERROR_MESSAGE_LIMIT as MESSAGE_LIMIT, Failure, Retry};

/// 上下文超长时各家服务常见的提示（不区分大小写）。
const TOO_LONG: [&str; 4] = [
    "context_length_exceeded",
    "maximum context length",
    "prompt is too long",
    "context window",
];

struct ApiError {
    kind: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

impl ApiError {
    fn parse(value: &Value) -> Self {
        let error = value
            .get("error")
            .filter(|e| e.is_object())
            .unwrap_or(value);
        let field = |name: &str| match error.get(name) {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        };
        Self {
            kind: field("type"),
            code: field("code"),
            message: field("message"),
        }
    }

    fn too_long(&self, raw: &str) -> bool {
        let raw = raw.to_lowercase();
        self.code.as_deref() == Some("context_length_exceeded")
            || TOO_LONG.iter().any(|needle| raw.contains(needle))
    }

    fn overloaded(&self, status: u16) -> bool {
        status == 529
            || self.code.as_deref() == Some("server_is_overloaded")
            || self.kind.as_deref() == Some("overloaded_error")
    }
}

pub(crate) fn from_response(
    status: u16,
    headers: &ResponseHeaders,
    body: &str,
    now: SystemTime,
) -> Failure {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let api = ApiError::parse(&value);
    let retry_after = retry_after(headers, now);
    let retry_after_ms =
        retry_after.map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
    let error = classify(status, &api, body, retry_after_ms);
    let retry = if api.overloaded(status) {
        Retry::Overloaded
    } else {
        match should_retry(headers) {
            Some(true) => Retry::Transient,
            Some(false) => Retry::Never,
            None if matches!(status, 401 | 408 | 409 | 429) || status >= 500 => Retry::Transient,
            None => Retry::Never,
        }
    };
    Failure {
        error,
        retry,
        retry_after,
    }
}

/// 流里带的错误对象（Chat 的 chunk.error、Responses 的 `error` / `response.failed`）。
pub(crate) fn from_stream(value: &Value) -> ModelError {
    let api = ApiError::parse(value);
    let raw = value.to_string();
    let status = match (api.kind.as_deref(), api.code.as_deref()) {
        (_, Some("rate_limit_exceeded" | "insufficient_quota")) | (Some("rate_limit_error"), _) => {
            429
        }
        (_, Some("server_is_overloaded")) | (Some("overloaded_error"), _) => 529,
        (Some("invalid_request_error"), _) => 400,
        (Some("authentication_error"), _) => 401,
        _ => 500,
    };
    classify(status, &api, &raw, None)
}

fn classify(status: u16, api: &ApiError, raw: &str, retry_after_ms: Option<u64>) -> ModelError {
    let message = message(status, api, raw);
    if api.too_long(raw) {
        return ModelError::PromptTooLong { message };
    }
    if api.overloaded(status) {
        return ModelError::Overloaded { message };
    }
    match status {
        429 => ModelError::RateLimited {
            message,
            retry_after_ms,
        },
        401 | 403 => ModelError::Authentication { message },
        400..=499 => ModelError::InvalidRequest { message },
        _ => ModelError::Server { status, message },
    }
}

fn message(status: u16, api: &ApiError, raw: &str) -> String {
    let text = match &api.message {
        Some(message) => message.as_str(),
        None => raw.trim(),
    };
    let text = match text.char_indices().nth(MESSAGE_LIMIT) {
        Some((end, _)) => &text[..end],
        None => text,
    };
    if text.is_empty() || text == "null" {
        format!("HTTP {status}")
    } else {
        redact_secrets(text).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use rust_cli_net::ResponseHeaders;
    use serde_json::json;

    use super::*;

    fn failure(status: u16, body: &str) -> Failure {
        // 这些用例不看 Retry-After 的日期：取一个固定时刻（F7 起产品代码不直接读系统时间，测试也不必）。
        from_response(
            status,
            &ResponseHeaders::default(),
            body,
            SystemTime::UNIX_EPOCH,
        )
    }

    #[test]
    fn statuses_and_codes_map_like_the_anthropic_table() {
        let too_long = failure(
            400,
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        );
        assert!(matches!(too_long.error, ModelError::PromptTooLong { .. }));
        assert_eq!(too_long.retry, Retry::Never);
        let limited = failure(
            429,
            r#"{"error":{"message":"slow down","type":"requests"}}"#,
        );
        assert!(matches!(limited.error, ModelError::RateLimited { .. }));
        assert_eq!(limited.retry, Retry::Transient);
        let busy = failure(
            503,
            r#"{"error":{"message":"busy","code":"server_is_overloaded"}}"#,
        );
        assert!(matches!(busy.error, ModelError::Overloaded { .. }));
        assert_eq!(busy.retry, Retry::Overloaded);
        let auth = failure(401, r#"{"error":{"message":"bad key"}}"#);
        assert!(
            matches!(auth.error, ModelError::Authentication { ref message } if message == "bad key")
        );
        assert_eq!(auth.retry, Retry::Transient);
        let bad = failure(404, "not json");
        assert!(
            matches!(bad.error, ModelError::InvalidRequest { ref message } if message == "not json")
        );
        assert_eq!(bad.retry, Retry::Never);
        let server = failure(500, "");
        assert!(
            matches!(server.error, ModelError::Server { status: 500, ref message } if message == "HTTP 500")
        );
    }

    #[test]
    fn stream_errors_use_the_error_type() {
        let error =
            from_stream(&json!({"error": {"message": "quota", "code": "insufficient_quota"}}));
        assert!(matches!(error, ModelError::RateLimited { .. }));
        let error = from_stream(&json!({"type": "error", "message": "boom"}));
        assert!(matches!(error, ModelError::Server { status: 500, .. }));
    }
}
