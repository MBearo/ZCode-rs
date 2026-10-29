//! Anthropic 协议的错误形状（docs/specs/rust-cli/model.md §5.1，provider-errors.md §5.2）。
//!
//! 正文是 `{"type":"error","error":{"type":…,"code"?:…,"message":…}}`。先认上下文超长："prompt is too long"（直连 API
//! 回 400，Vertex 回 413，大小写不一），4xx 另认共用词表里的说法（§5.1.1，修复线 F108）；再认过载（529，或正文里有
//! `"type":"overloaded_error"`——SDK 在流式请求里有时拿不到 529 状态码）；其余交给通用规则（配额、拦截、太大、状态码）。
//! 能否重试不在这里定（`dispose`，修复线 F113）。

use rust_cli_kernel_api::model::ModelError;
use serde_json::Value;

use super::{Class, Shape, Verdict, by_status, message_text, vocab};

/// 非 2xx 响应。
pub(super) fn response(
    status: u16,
    value: &Value,
    raw: &str,
    retry_after_ms: Option<u64>,
) -> Verdict {
    classify(status, &ApiError::parse(value), raw, retry_after_ms)
}

/// 流里的错误块（HTTP 状态已是 200）：按错误类型对应的状态码分类。`raw` 是块的原文。
pub(super) fn stream(value: &Value, raw: &str) -> Verdict {
    let api = ApiError::parse(value);
    classify(status_of(&api), &api, raw, None)
}

/// 正文里的三项。修复线 F113：补上 `error.code`（以前只取 `type` 与 `message`，兼容端点放在 `code` 里的码丢了）。
struct ApiError {
    kind: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

impl ApiError {
    fn parse(value: &Value) -> Self {
        let error = value.get("error").unwrap_or(value);
        let field = |name: &str| match error.get(name) {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        };
        Self {
            kind: field("type"),
            code: field("code"),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }

    fn codes(&self) -> [Option<&str>; 2] {
        [self.kind.as_deref(), self.code.as_deref()]
    }
}

fn is_overloaded(status: u16, api: &ApiError, raw: &str) -> bool {
    status == 529
        || api
            .codes()
            .iter()
            .flatten()
            .any(|code| vocab::OVERLOADED_CODES.contains(code))
        || raw.contains(r#""type":"overloaded_error""#)
}

fn classify(status: u16, api: &ApiError, raw: &str, retry_after_ms: Option<u64>) -> Verdict {
    let message = message_text(status, api.message.as_deref(), raw);
    // 修 bug（F108）：兼容端点用 OpenAI 的句式报超长（例如 400 `This model's maximum context length is …`），以前归成
    // `InvalidRequest`、不触发反应式压缩。共用词表只在 4xx 上看（5xx 的正文不是对这次请求的判定）；`prompt is too long`
    // 照旧不看状态码。
    let too_long = raw.to_lowercase().contains("prompt is too long")
        || ((400..500).contains(&status) && vocab::context_exceeded(raw));
    if too_long {
        return Verdict::new(
            Class::ContextOverflow,
            ModelError::PromptTooLong { message },
        );
    }
    if is_overloaded(status, api, raw) {
        return Verdict::new(Class::Overloaded, ModelError::Overloaded { message });
    }
    let shape = Shape {
        message,
        retry_after_ms,
    };
    by_status(status, &api.codes(), raw, shape)
}

/// Anthropic 错误类型对应的 HTTP 状态（API 文档的错误表）；未知类型按 500。别家的限流、过载、配额码也认（通用词表）。
fn status_of(api: &ApiError) -> u16 {
    let has = |table: &[&str]| {
        api.codes()
            .iter()
            .flatten()
            .any(|code| table.contains(code))
    };
    if has(&vocab::OVERLOADED_CODES) {
        return 529;
    }
    if has(&vocab::QUOTA_CODES) {
        return 402;
    }
    if has(&vocab::RATE_LIMIT_CODES) {
        return 429;
    }
    match api.kind.as_deref() {
        Some("invalid_request_error") => 400,
        Some("authentication_error") => 401,
        Some("permission_error") => 403,
        Some("not_found_error") => 404,
        Some("request_too_large") => 413,
        Some("timeout_error") => 504,
        _ => 500,
    }
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
