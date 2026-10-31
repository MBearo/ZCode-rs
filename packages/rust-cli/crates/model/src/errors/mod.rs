//! 模型请求失败的分类（docs/specs/rust-cli/provider-errors.md §2，修复线 F113）。
//!
//! 第 1 层：按端点选厂商（`profiles::vendor`），厂商层先认自己的码与结束原因，认不出再交给协议层（Anthropic / OpenAI 的
//! 正文形状），最后是通用规则（词表、配额、媒体、状态码表）。输出中性的 [`Verdict`]：分类交给第 2 层（[`dispose`]）决定
//! 处置，`ModelError` 交给调用方。厂商码与厂商专有的字符串只在这一层（`check:rust-cli` 的解耦检测）。
//!
//! 四种到达形式各有一个入口：响应头之前的失败 [`classify_response`]、流里的错误块 [`classify_stream_error`]、结束原因
//! [`classify_finish`]、传输错误 [`classify_transport`]。都是纯函数，没有 IO。

mod anthropic;
pub(crate) mod dispose;
mod openai;
mod vocab;
mod zhipu;

use std::time::{Duration, SystemTime};

use rust_cli_base::proto::http;
use rust_cli_base::redact::redact_secrets;
use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::retry::{retry_after, should_retry};
use rust_cli_net::{NetError, ResponseHeaders};
use serde_json::Value;

/// 错误正文里给出的消息最多保留这么多字符（各协议与厂商相同，F55：以前各写一份）。
pub(crate) const ERROR_MESSAGE_LIMIT: usize = 2000;
/// 服务过载的状态码（非标准的 529，Anthropic 与兼容端点用它）。
const OVERLOADED_STATUS: u16 = 529;

/// 错误码表的厂商（provider-errors.md §2.2）：只看端点，不看模型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Vendor {
    /// 没有厂商层：只按协议与通用规则。
    #[default]
    Generic,
    /// 智谱的端点（含 ZCode 套餐网关）。
    Zhipu,
}

/// 中性分类（provider-errors.md §2.3）。第 2 层与 query 只认它与 `ModelError`，不认厂商码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Overloaded,
    RateLimited,
    /// 账号或套餐的并发上限。
    CapacityLimited,
    ServerError,
    Network,
    TlsFailure,
    AuthRejected,
    AccessDenied,
    QuotaExhausted,
    ContentBlocked,
    ContextOverflow,
    TooLarge,
    InvalidRequest,
    /// 流的协议违例。
    Malformed,
    /// 流读完却没有收尾。
    Truncated,
}

/// 一次失败的分类结果：中性分类、交给调用方的错误与响应头里的两项。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verdict {
    pub class: Class,
    pub error: ModelError,
    /// Retry-After。
    pub retry_after: Option<Duration>,
    /// `x-should-retry`。
    pub should_retry: Option<bool>,
}

impl Verdict {
    pub(crate) fn new(class: Class, error: ModelError) -> Self {
        Self {
            class,
            error,
            retry_after: None,
            should_retry: None,
        }
    }

    /// 流的协议违例（块对不上、JSON 无效）。
    pub(crate) fn malformed(message: impl Into<String>) -> Self {
        Self::new(
            Class::Malformed,
            ModelError::Protocol {
                message: message.into(),
            },
        )
    }

    /// 流读完却没有收到终止事件。
    pub(crate) fn truncated() -> Self {
        Self::new(
            Class::Truncated,
            ModelError::Protocol {
                message: "stream ended before message_stop".to_owned(),
            },
        )
    }
}

/// 结束原因的换算结果：多数是停止原因，少数厂商用结束原因报错（provider-errors.md §2.3）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Finish {
    Stop(StopReason),
    Failed(Verdict),
}

/// 非 2xx 响应（到达形式 ①）。`body` 是截断后的正文；`now` 取自注入的时钟（Retry-After 的 HTTP 日期，F7）。
pub(crate) fn classify_response(
    vendor: Vendor,
    protocol: WireProtocol,
    (status, headers): (u16, &ResponseHeaders),
    body: &str,
    now: SystemTime,
) -> Verdict {
    let retry_after = retry_after(headers, now);
    let retry_after_ms =
        retry_after.map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let found = match vendor {
        Vendor::Zhipu => zhipu::response(status, &value, body, retry_after_ms),
        Vendor::Generic => None,
    };
    let mut verdict = found.unwrap_or_else(|| match protocol {
        WireProtocol::AnthropicMessages => {
            anthropic::response(status, &value, body, retry_after_ms)
        }
        WireProtocol::OpenAiChatCompletions | WireProtocol::OpenAiResponses => {
            openai::response(status, &value, body, retry_after_ms)
        }
    });
    verdict.retry_after = retry_after;
    verdict.should_retry = should_retry(headers);
    verdict
}

/// HTTP 200 之后流里的错误块（到达形式 ②）：Anthropic 的 `event: error`、OpenAI 的 `error` / `response.failed`。
/// `raw` 是块的原文（解析不了时 `value` 是 `Null`）。
pub(crate) fn classify_stream_error(
    vendor: Vendor,
    protocol: WireProtocol,
    value: &Value,
    raw: &str,
) -> Verdict {
    let found = match vendor {
        Vendor::Zhipu => zhipu::stream(value, raw),
        Vendor::Generic => None,
    };
    found.unwrap_or_else(|| match protocol {
        WireProtocol::AnthropicMessages => anthropic::stream(value, raw),
        WireProtocol::OpenAiChatCompletions | WireProtocol::OpenAiResponses => {
            openai::stream(value, raw)
        }
    })
}

/// 不带事件名的 SSE 块是不是错误（provider-errors.md D20）：正文是 `{"type":"error",…}` 或带非空的 `error`；
/// 厂商层另认自己的写法。不是错误时为 None（调用方照旧忽略这个块）。
pub(crate) fn unnamed_frame(
    vendor: Vendor,
    protocol: WireProtocol,
    value: &Value,
    raw: &str,
) -> Option<Verdict> {
    if vendor == Vendor::Zhipu
        && let Some(verdict) = zhipu::frame(value, raw)
    {
        return Some(verdict);
    }
    let error_shaped = value.get("type").and_then(Value::as_str) == Some("error")
        || value.get("error").is_some_and(|error| !error.is_null());
    error_shaped.then(|| classify_stream_error(vendor, protocol, value, raw))
}

/// 结束原因的原值（到达形式 ③）：Anthropic 的 `stop_reason`、OpenAI 的 `finish_reason` 与
/// `incomplete_details.reason`。厂商层先认，其余按一张通用表；不认识的是 `Unknown`。
pub(crate) fn classify_finish(vendor: Vendor, raw: &str) -> Finish {
    if vendor == Vendor::Zhipu
        && let Some(finish) = zhipu::finish(raw)
    {
        return finish;
    }
    Finish::Stop(match raw {
        "end_turn" | "stop" => StopReason::EndTurn,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" | "tool_calls" | "function_call" => StopReason::ToolUse,
        "max_tokens" | "length" | "max_output_tokens" => StopReason::MaxTokens,
        "model_context_window_exceeded" => StopReason::ModelContextWindowExceeded,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" | "content_filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    })
}

/// 传输错误（到达形式 ④）：建连、超时、读写中断可以重试；TLS 与无效请求不重试（处置在 [`dispose`]）。
pub(crate) fn classify_transport(error: &NetError) -> Verdict {
    match error {
        NetError::Request(message) | NetError::Policy(message) => Verdict::new(
            Class::InvalidRequest,
            ModelError::InvalidRequest {
                message: message.clone(),
            },
        ),
        NetError::Tls(_) => Verdict::new(
            Class::TlsFailure,
            ModelError::Network {
                message: error.to_string(),
            },
        ),
        NetError::Connect(_) | NetError::Timeout | NetError::Io(_) => Verdict::new(
            Class::Network,
            ModelError::Network {
                message: error.to_string(),
            },
        ),
    }
}

/// 端点给的消息；没有时用截断的正文，正文也为空时用状态码。按值脱敏，最多 [`ERROR_MESSAGE_LIMIT`] 个字符。
fn message_text(status: u16, given: Option<&str>, raw: &str) -> String {
    let text = given.unwrap_or_else(|| raw.trim());
    let text = match text.char_indices().nth(ERROR_MESSAGE_LIMIT) {
        Some((end, _)) => &text[..end],
        None => text,
    };
    if text.is_empty() || text == "null" {
        format!("HTTP {status}")
    } else {
        redact_secrets(text).into_owned()
    }
}

/// 协议层认过超长与过载之后的通用规则（provider-errors.md §5.1）：配额、内容拦截、太大，再按状态码。
/// `codes` 是正文里的错误类型与错误码。
fn by_status(status: u16, codes: &[Option<&str>], raw: &str, error: Shape) -> Verdict {
    let Shape {
        message,
        retry_after_ms,
    } = error;
    let has = |table: &[&str]| codes.iter().flatten().any(|code| table.contains(code));
    if status == http::PAYMENT_REQUIRED || has(&vocab::QUOTA_CODES) {
        return Verdict::new(
            Class::QuotaExhausted,
            ModelError::QuotaExhausted { message },
        );
    }
    if has(&vocab::CONTENT_FILTER_CODES) {
        return Verdict::new(
            Class::ContentBlocked,
            ModelError::ContentBlocked { message },
        );
    }
    let client = http::is_client_error(status);
    if status == http::PAYLOAD_TOO_LARGE
        || has(&vocab::TOO_LARGE_CODES)
        || (client && vocab::media_too_large(raw))
    {
        return Verdict::new(Class::TooLarge, ModelError::RequestTooLarge { message });
    }
    if status == http::TOO_MANY_REQUESTS || has(&vocab::RATE_LIMIT_CODES) {
        return Verdict::new(
            Class::RateLimited,
            ModelError::RateLimited {
                message,
                retry_after_ms,
            },
        );
    }
    match status {
        http::UNAUTHORIZED => {
            Verdict::new(Class::AuthRejected, ModelError::Authentication { message })
        }
        http::FORBIDDEN => {
            Verdict::new(Class::AccessDenied, ModelError::Authentication { message })
        }
        http::NOT_FOUND => {
            Verdict::new(Class::AccessDenied, ModelError::InvalidRequest { message })
        }
        // 408、409 照旧是 `InvalidRequest` 的形状，处置按服务端临时错误重试（只改处置，不改形状）。
        http::REQUEST_TIMEOUT | http::CONFLICT => {
            Verdict::new(Class::ServerError, ModelError::InvalidRequest { message })
        }
        http::BAD_REQUEST..http::INTERNAL_SERVER_ERROR => Verdict::new(
            Class::InvalidRequest,
            ModelError::InvalidRequest { message },
        ),
        _ => Verdict::new(Class::ServerError, ModelError::Server { status, message }),
    }
}

/// 构造错误要的两项。
struct Shape {
    message: String,
    retry_after_ms: Option<u64>,
}

/// 厂商层按码给出分类时的错误形状（provider-errors.md §2.3 的最后一列）。`status` 是 HTTP 状态；流里的错误没有状态。
fn error_of(class: Class, status: Option<u16>, error: Shape) -> ModelError {
    let Shape {
        message,
        retry_after_ms,
    } = error;
    match class {
        Class::Overloaded => ModelError::Overloaded { message },
        Class::RateLimited | Class::CapacityLimited => ModelError::RateLimited {
            message,
            retry_after_ms,
        },
        Class::ServerError => ModelError::Server {
            status: status
                .filter(|status| *status >= http::SERVER_ERROR_MIN)
                .unwrap_or(http::INTERNAL_SERVER_ERROR),
            message,
        },
        Class::Network | Class::TlsFailure => ModelError::Network { message },
        Class::AuthRejected => ModelError::Authentication { message },
        Class::AccessDenied if matches!(status, Some(http::UNAUTHORIZED | http::FORBIDDEN)) => {
            ModelError::Authentication { message }
        }
        Class::AccessDenied | Class::InvalidRequest => ModelError::InvalidRequest { message },
        Class::QuotaExhausted => ModelError::QuotaExhausted { message },
        Class::ContentBlocked => ModelError::ContentBlocked { message },
        Class::ContextOverflow => ModelError::PromptTooLong { message },
        Class::TooLarge => ModelError::RequestTooLarge { message },
        Class::Malformed | Class::Truncated => ModelError::Protocol { message },
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
