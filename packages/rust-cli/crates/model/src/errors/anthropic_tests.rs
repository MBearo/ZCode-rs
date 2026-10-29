//! Anthropic 协议的错误形状：中性分类与交给调用方的错误（能否重试在 `dispose_tests.rs`）。

use std::time::{Duration, UNIX_EPOCH};

use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::ResponseHeaders;

use crate::errors::{Class, ERROR_MESSAGE_LIMIT as MESSAGE_LIMIT, Vendor, Verdict};

fn api_error(kind: &str, message: &str) -> String {
    serde_json::json!({"type": "error", "error": {"type": kind, "message": message}}).to_string()
}

fn failure(status: u16, headers: &[(&str, &str)], body: &str) -> Verdict {
    crate::errors::classify_response(
        Vendor::Generic,
        WireProtocol::AnthropicMessages,
        (
            status,
            &ResponseHeaders::from_pairs(headers.iter().copied()),
        ),
        body,
        UNIX_EPOCH,
    )
}

fn from_stream(data: &str) -> ModelError {
    let value = serde_json::from_str(data).unwrap_or(serde_json::Value::Null);
    crate::errors::classify_stream_error(
        Vendor::Generic,
        WireProtocol::AnthropicMessages,
        &value,
        data,
    )
    .error
}

#[test]
fn status_codes_map_to_error_kinds_and_classes() {
    let rate = failure(
        429,
        &[("retry-after", "7")],
        &api_error("rate_limit_error", "slow down"),
    );
    assert_eq!(
        rate,
        Verdict {
            class: Class::RateLimited,
            error: ModelError::RateLimited {
                message: "slow down".to_owned(),
                retry_after_ms: Some(7000),
            },
            retry_after: Some(Duration::from_secs(7)),
            should_retry: None,
        }
    );
    let overloaded = failure(529, &[], &api_error("overloaded_error", "Overloaded"));
    assert_eq!(
        overloaded.error,
        ModelError::Overloaded {
            message: "Overloaded".to_owned()
        }
    );
    assert_eq!(overloaded.class, Class::Overloaded);
    let unauthorized = failure(
        401,
        &[],
        &api_error("authentication_error", "invalid x-api-key"),
    );
    assert!(matches!(
        unauthorized.error,
        ModelError::Authentication { .. }
    ));
    assert_eq!(unauthorized.class, Class::AuthRejected);
    let forbidden = failure(403, &[], &api_error("permission_error", "no"));
    assert!(matches!(forbidden.error, ModelError::Authentication { .. }));
    assert_eq!(forbidden.class, Class::AccessDenied);
    let missing = failure(404, &[], &api_error("not_found_error", "model: x"));
    assert!(matches!(missing.error, ModelError::InvalidRequest { .. }));
    assert_eq!(missing.class, Class::AccessDenied);
    let bad = failure(
        400,
        &[],
        &api_error("invalid_request_error", "messages: required"),
    );
    assert!(matches!(bad.error, ModelError::InvalidRequest { .. }));
    assert_eq!(bad.class, Class::InvalidRequest);
    // 408、409 照旧是参数错误的形状，处置按服务端临时错误（重试）。
    for status in [408, 409] {
        let got = failure(status, &[], "");
        assert!(matches!(got.error, ModelError::InvalidRequest { .. }));
        assert_eq!(got.class, Class::ServerError);
    }
    let server = failure(503, &[], "upstream connect error");
    assert_eq!(
        server.error,
        ModelError::Server {
            status: 503,
            message: "upstream connect error".to_owned()
        }
    );
    assert_eq!(server.class, Class::ServerError);
    assert_eq!(
        failure(502, &[], "  ").error,
        ModelError::Server {
            status: 502,
            message: "HTTP 502".to_owned()
        }
    );
}

#[test]
fn prompt_too_long_is_recognized_on_400_and_413() {
    let direct = failure(
        400,
        &[],
        &api_error(
            "invalid_request_error",
            "prompt is too long: 210000 tokens > 200000 maximum",
        ),
    );
    assert_eq!(
        direct.error,
        ModelError::PromptTooLong {
            message: "prompt is too long: 210000 tokens > 200000 maximum".to_owned()
        }
    );
    assert_eq!(direct.class, Class::ContextOverflow);
    // Vertex：413，首字母大写，正文不是 Anthropic 的错误形状。
    let vertex = failure(413, &[], "Prompt is too long");
    assert!(matches!(vertex.error, ModelError::PromptTooLong { .. }));
}

#[test]
fn overload_in_the_body_wins_over_the_status_and_headers() {
    // SDK 在流式请求里有时拿不到 529：正文里的 overloaded_error 也算过载，且不受 x-should-retry 影响。
    let hidden = failure(
        500,
        &[("x-should-retry", "false")],
        &api_error("overloaded_error", "Overloaded"),
    );
    assert!(matches!(hidden.error, ModelError::Overloaded { .. }));
    assert_eq!(hidden.class, Class::Overloaded);
    assert_eq!(hidden.should_retry, Some(false));
}

/// `x-should-retry` 原样带出，处置表按它（`dispose_tests.rs`）。
#[test]
fn x_should_retry_is_carried_in_the_verdict() {
    let never = failure(
        500,
        &[("x-should-retry", "false")],
        &api_error("api_error", "boom"),
    );
    assert_eq!(never.should_retry, Some(false));
    let always = failure(
        400,
        &[("x-should-retry", "true")],
        &api_error("invalid_request_error", "x"),
    );
    assert_eq!(always.should_retry, Some(true));
    assert_eq!(failure(500, &[], "").should_retry, None);
}

#[test]
fn messages_are_truncated_and_redacted() {
    let long = "x".repeat(MESSAGE_LIMIT + 10);
    let Verdict { error, .. } = failure(500, &[], &long);
    let ModelError::Server { message, .. } = error else {
        panic!("{error:?}");
    };
    assert_eq!(message.chars().count(), MESSAGE_LIMIT);
    let leaked = failure(
        401,
        &[],
        &api_error(
            "authentication_error",
            "bad key sk-ant-api03-abcdefghijklmnopqrstuv",
        ),
    );
    assert!(
        !leaked.error.to_string().contains("abcdefghijklmnop"),
        "{:?}",
        leaked.error
    );
}

#[test]
fn stream_errors_use_the_status_of_their_type() {
    assert_eq!(
        from_stream(&api_error("overloaded_error", "Overloaded")),
        ModelError::Overloaded {
            message: "Overloaded".to_owned()
        }
    );
    assert_eq!(
        from_stream(&api_error("api_error", "Internal server error")),
        ModelError::Server {
            status: 500,
            message: "Internal server error".to_owned()
        }
    );
    assert!(matches!(
        from_stream(&api_error("rate_limit_error", "later")),
        ModelError::RateLimited {
            retry_after_ms: None,
            ..
        }
    ));
    assert!(matches!(
        from_stream(&api_error(
            "invalid_request_error",
            "prompt is too long: 5 tokens > 4 maximum"
        )),
        ModelError::PromptTooLong { .. }
    ));
    assert!(matches!(
        from_stream("not json"),
        ModelError::Server { status: 500, .. }
    ));
}

/// 兼容端点回 OpenAI 句式的超长（修复线 F108，model.md §5.1.1）：400 的原文。
const COMPATIBLE_OVERFLOW: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1055008 tokens (671008 in the messages, 384000 in the completion).";

/// 兼容端点的超长说法也是上下文超限（触发反应式压缩），消息是原文、不重试。正文是 Anthropic 的错误形状或
/// OpenAI 的错误形状都认。
#[test]
fn an_openai_style_overflow_from_a_compatible_endpoint_is_prompt_too_long() {
    let anthropic_shape = failure(
        400,
        &[],
        &api_error("invalid_request_error", COMPATIBLE_OVERFLOW),
    );
    assert_eq!(
        anthropic_shape.error,
        ModelError::PromptTooLong {
            message: COMPATIBLE_OVERFLOW.to_owned()
        }
    );
    assert_eq!(anthropic_shape.class, Class::ContextOverflow);
    let openai_shape = serde_json::json!({"error": {"message": COMPATIBLE_OVERFLOW,
        "type": "invalid_request_error", "param": null, "code": "invalid_request_error"}})
    .to_string();
    assert!(matches!(
        failure(400, &[], &openai_shape).error,
        ModelError::PromptTooLong { .. }
    ));
    // 流里的错误事件（`invalid_request_error` 按 400 算）。
    assert!(matches!(
        from_stream(&api_error("invalid_request_error", COMPATIBLE_OVERFLOW)),
        ModelError::PromptTooLong { .. }
    ));
}

/// 共用词表里另外几家服务的说法与错误码。
#[test]
fn the_shared_vocabulary_covers_other_providers() {
    for message in [
        "The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).",
        "Range of input length should be [1, 129024]",
        "Total message token length exceed model limit",
        "context_window_exceeded",
        "model_context_window_exceeded",
        "Input exceeds the context window of this model",
    ] {
        let got = failure(400, &[], &api_error("invalid_request_error", message));
        assert!(
            matches!(got.error, ModelError::PromptTooLong { .. }),
            "{message}: {:?}",
            got.error
        );
    }
    let coded = serde_json::json!({"error": {"message": "too many tokens", "code": "context_length_exceeded"}})
        .to_string();
    assert!(matches!(
        failure(400, &[], &coded).error,
        ModelError::PromptTooLong { .. }
    ));
}

/// 不误伤：`invalid_request_error` 里别的内容照旧是 `InvalidRequest`；词表只在 4xx 上看，5xx 里出现同样的说法仍是服务端
/// 错误（`prompt is too long` 照旧不看状态码）。
#[test]
fn other_invalid_requests_are_not_mistaken_for_overflow() {
    for message in [
        "max_tokens: 400000 > 128000, which is the maximum allowed number of output tokens for this model",
        "messages: text content blocks must be non-empty",
    ] {
        let got = failure(400, &[], &api_error("invalid_request_error", message));
        assert!(
            matches!(got.error, ModelError::InvalidRequest { .. }),
            "{message}: {:?}",
            got.error
        );
    }
    let server = failure(500, &[], &api_error("api_error", COMPATIBLE_OVERFLOW));
    assert!(matches!(
        server.error,
        ModelError::Server { status: 500, .. }
    ));
    let stream = from_stream(&api_error("api_error", COMPATIBLE_OVERFLOW));
    assert!(matches!(stream, ModelError::Server { status: 500, .. }));
    // `prompt is too long` 不看状态码（与修复前相同）。
    let legacy = failure(
        500,
        &[],
        &api_error("api_error", "prompt is too long: 5 tokens > 4 maximum"),
    );
    assert!(matches!(legacy.error, ModelError::PromptTooLong { .. }));
}

/// 修改（provider-errors.md D8 暂定 (b)）：输入加输出上限超出窗口以前是 `InvalidRequest`，回合直接失败；现在按上下文超限
/// 压缩，不调小重发。
#[test]
fn input_plus_output_over_the_window_is_an_overflow() {
    let message = "input length and `max_tokens` exceed context limit: 197000 + 21333 > 200000, decrease input length or `max_tokens` and try again";
    let got = failure(400, &[], &api_error("invalid_request_error", message));
    assert_eq!(got.class, Class::ContextOverflow);
    assert_eq!(
        got.error,
        ModelError::PromptTooLong {
            message: message.to_owned()
        }
    );
}

/// 修改（D9 暂定）：413、`request_too_large` 与图片过大以前是 `InvalidRequest`（文字 `invalid request: …`）；现在是请求太大，
/// 终止并提示去掉图片或回退。
#[test]
fn oversized_requests_and_media_are_too_large() {
    for (status, body) in [
        (
            413,
            api_error("request_too_large", "Request exceeds the maximum size"),
        ),
        (
            400,
            api_error(
                "invalid_request_error",
                "messages.0.content.1.image.source.base64: image exceeds 5 MB maximum: 7340032 bytes > 5242880 bytes",
            ),
        ),
        (
            400,
            api_error(
                "invalid_request_error",
                "messages.0.content.1.image.source.base64.data: At least one of the image dimensions exceed max allowed size for many-image requests: 2000 pixels",
            ),
        ),
    ] {
        let got = failure(status, &[], &body);
        assert_eq!(got.class, Class::TooLarge, "{body}");
        assert!(
            matches!(got.error, ModelError::RequestTooLarge { .. }),
            "{:?}",
            got.error
        );
    }
    assert!(matches!(
        from_stream(&api_error("request_too_large", "too big")),
        ModelError::RequestTooLarge { .. }
    ));
    // 5xx 里的同样说法不是对这次请求的判定。
    let server = failure(
        500,
        &[],
        &api_error("api_error", "image exceeds 5 MB maximum"),
    );
    assert_eq!(server.class, Class::ServerError);
}

/// 修改（D4 暂定）：402 / `billing_error` 以前是 `InvalidRequest`；通用的配额码以前按状态（429 会重试十次）。现在都是配额
/// 用尽，不重试。
#[test]
fn billing_and_quota_codes_are_quota_exhausted() {
    let billing = failure(
        402,
        &[],
        &api_error("billing_error", "credit balance is too low"),
    );
    assert_eq!(billing.class, Class::QuotaExhausted);
    assert_eq!(
        billing.error,
        ModelError::QuotaExhausted {
            message: "credit balance is too low".to_owned()
        }
    );
    let coded = serde_json::json!({"type": "error", "error": {"type": "rate_limit_error",
        "code": "insufficient_quota", "message": "quota"}})
    .to_string();
    assert_eq!(failure(429, &[], &coded).class, Class::QuotaExhausted);
    assert!(matches!(
        from_stream(&api_error("billing_error", "no credit")),
        ModelError::QuotaExhausted { .. }
    ));
}

/// 流里不带事件名的错误块按错误分类（provider-errors.md D20）；正常的块不算。
#[test]
fn unnamed_error_frames_are_errors() {
    let frame = |data: &str| {
        let value: serde_json::Value = serde_json::from_str(data).unwrap();
        crate::errors::unnamed_frame(
            Vendor::Generic,
            WireProtocol::AnthropicMessages,
            &value,
            data,
        )
    };
    let typed = frame(&api_error("overloaded_error", "Overloaded")).unwrap();
    assert_eq!(typed.class, Class::Overloaded);
    let bare = frame(r#"{"error":{"type":"api_error","message":"boom"}}"#).unwrap();
    assert_eq!(bare.class, Class::ServerError);
    assert!(frame(r#"{"type":"content_block_delta","index":0}"#).is_none());
    assert!(frame(r#"{"error":null,"type":"ping"}"#).is_none());
}
