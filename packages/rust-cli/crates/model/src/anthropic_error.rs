//! Anthropic 错误分类（docs/specs/rust-cli/model.md §5.1）。
//!
//! 先认上下文超长："prompt is too long"（直连 API 回 400，Vertex 回 413，大小写不一），4xx 另认共用词表里的说法
//! （§5.1.1，修复线 F108），再认过载（529，或正文里有 `"type":"overloaded_error"`——SDK 在
//! 流式请求里有时拿不到 529 状态码），再按状态码分类。

use std::time::SystemTime;

use rust_cli_base::redact::redact_secrets;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_net::ResponseHeaders;
use rust_cli_net::retry::{retry_after, should_retry};
use serde_json::Value;

// 消息的长度上限两种协议的错误共用（F55）。
use crate::overflow;
use crate::protocol::{ERROR_MESSAGE_LIMIT as MESSAGE_LIMIT, Failure, Retry};

/// 非 2xx 响应。
pub(crate) fn from_response(
    status: u16,
    headers: &ResponseHeaders,
    body: &str,
    now: SystemTime,
) -> Failure {
    let api = ApiError::parse(body);
    let retry_after = retry_after(headers, now);
    let retry_after_ms =
        retry_after.map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
    let error = classify(status, &api, body, retry_after_ms);
    // 能否重试的顺序：过载 → x-should-retry → 状态码。
    let retry = if is_overloaded(status, &api, body) {
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

/// 流里的 `error` 事件（HTTP 状态已是 200）：按错误类型对应的状态码分类。
pub(crate) fn from_stream(data: &str) -> ModelError {
    let api = ApiError::parse(data);
    classify(status_of(api.kind.as_deref()), &api, data, None)
}

/// 正文 `{"type":"error","error":{"type":…,"message":…}}` 里的两项。
struct ApiError {
    kind: Option<String>,
    message: Option<String>,
}

impl ApiError {
    fn parse(body: &str) -> Self {
        let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let error = value.get("error").unwrap_or(&value);
        let field = |name: &str| error.get(name).and_then(Value::as_str).map(str::to_owned);
        Self {
            kind: field("type"),
            message: field("message"),
        }
    }
}

fn is_overloaded(status: u16, api: &ApiError, raw: &str) -> bool {
    status == 529
        || api.kind.as_deref() == Some("overloaded_error")
        || raw.contains(r#""type":"overloaded_error""#)
}

fn classify(status: u16, api: &ApiError, raw: &str, retry_after_ms: Option<u64>) -> ModelError {
    let message = message(status, api, raw);
    // 修 bug（F108）：兼容端点用 OpenAI 的句式报超长（例如 400 `This model's maximum context length is …`），以前归成
    // `InvalidRequest`、不触发反应式压缩。共用词表只在 4xx 上看（5xx 的正文不是对这次请求的判定）；`prompt is too long`
    // 照旧不看状态码。
    let too_long = raw.to_lowercase().contains("prompt is too long")
        || ((400..500).contains(&status) && overflow::context_exceeded(raw));
    if too_long {
        return ModelError::PromptTooLong { message };
    }
    if is_overloaded(status, api, raw) {
        return ModelError::Overloaded { message };
    }
    match status {
        429 => ModelError::RateLimited {
            message,
            retry_after_ms,
        },
        401 | 403 => ModelError::Authentication { message },
        // 413 是请求体过大（例如大 PDF 加上下文超过 32 MB）；含 "prompt is too long" 的已在上面处理。
        400..=499 => ModelError::InvalidRequest { message },
        _ => ModelError::Server { status, message },
    }
}

/// API 给的消息；没有时用截断的正文，正文也为空时用状态码。按值脱敏。
fn message(status: u16, api: &ApiError, raw: &str) -> String {
    let text = match &api.message {
        Some(message) => message.as_str(),
        None => raw.trim(),
    };
    let text = match text.char_indices().nth(MESSAGE_LIMIT) {
        Some((end, _)) => &text[..end],
        None => text,
    };
    if text.is_empty() {
        format!("HTTP {status}")
    } else {
        redact_secrets(text).into_owned()
    }
}

/// Anthropic 错误类型对应的 HTTP 状态（API 文档的错误表）；未知类型按 500。
fn status_of(kind: Option<&str>) -> u16 {
    match kind {
        Some("invalid_request_error") => 400,
        Some("authentication_error") => 401,
        Some("billing_error") => 402,
        Some("permission_error") => 403,
        Some("not_found_error") => 404,
        Some("request_too_large") => 413,
        Some("rate_limit_error") => 429,
        Some("timeout_error") => 504,
        Some("overloaded_error") => 529,
        _ => 500,
    }
}

#[cfg(test)]
#[path = "anthropic_error_tests.rs"]
mod tests;
