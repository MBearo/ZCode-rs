//! OpenAI 两种协议的错误形状（docs/specs/rust-cli/model.md §10.4，provider-errors.md §5.4）。
//!
//! 正文是 `{"error":{"message","type","code"}}`。分类沿用 Anthropic 的表（model.md §5.1），只换识别条件：
//! 上下文超长认与 Anthropic 一侧共用的词表（model.md §5.1.1，修复线 F108，不看状态码）；过载认 529 与过载码；其余交给
//! 通用规则（配额、拦截、太大、状态码）。能否重试不在这里定（`dispose`，修复线 F113）。

use rust_cli_kernel_api::model::ModelError;
use serde_json::Value;

use super::{Class, Shape, Verdict, by_status, message_text, vocab};

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

    fn codes(&self) -> [Option<&str>; 2] {
        [self.kind.as_deref(), self.code.as_deref()]
    }

    fn has(&self, table: &[&str]) -> bool {
        self.codes()
            .iter()
            .flatten()
            .any(|code| table.contains(code))
    }

    fn overloaded(&self, status: u16) -> bool {
        status == 529 || self.has(&vocab::OVERLOADED_CODES)
    }
}

/// 非 2xx 响应。
pub(super) fn response(
    status: u16,
    value: &Value,
    raw: &str,
    retry_after_ms: Option<u64>,
) -> Verdict {
    classify(status, &ApiError::parse(value), raw, retry_after_ms)
}

/// 流里带的错误对象（Chat 的 chunk.error、Responses 的 `error` / `response.failed`）：按类型与码对应的状态分类。
///
/// 修改（F113，provider-errors.md D4 暂定）：`insufficient_quota` 以前按 429 算、会重试十次；现在是配额用尽，不重试。
pub(super) fn stream(value: &Value, raw: &str) -> Verdict {
    let api = ApiError::parse(value);
    let status = if api.has(&vocab::QUOTA_CODES) {
        402
    } else if api.has(&vocab::RATE_LIMIT_CODES) {
        429
    } else if api.has(&vocab::OVERLOADED_CODES) {
        529
    } else {
        match api.kind.as_deref() {
            Some("invalid_request_error") => 400,
            Some("authentication_error") => 401,
            _ => 500,
        }
    };
    classify(status, &api, raw, None)
}

fn classify(status: u16, api: &ApiError, raw: &str, retry_after_ms: Option<u64>) -> Verdict {
    let message = message_text(status, api.message.as_deref(), raw);
    // 修改（F108）：以前是这里自己的四项，现在与 Anthropic 一侧共用词表（码在正文里，按子串一样认得出）。
    if vocab::context_exceeded(raw) {
        return Verdict::new(
            Class::ContextOverflow,
            ModelError::PromptTooLong { message },
        );
    }
    if api.overloaded(status) {
        return Verdict::new(Class::Overloaded, ModelError::Overloaded { message });
    }
    let shape = Shape {
        message,
        retry_after_ms,
    };
    by_status(status, &api.codes(), raw, shape)
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod tests;
