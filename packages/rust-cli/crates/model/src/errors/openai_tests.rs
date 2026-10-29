//! OpenAI 两种协议的错误形状（能否重试在 `dispose_tests.rs`）。

use std::time::SystemTime;

use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::ResponseHeaders;
use serde_json::{Value, json};

use crate::errors::{Class, Vendor, Verdict};

fn failure(status: u16, body: &str) -> Verdict {
    // 这些用例不看 Retry-After 的日期：取一个固定时刻（F7 起产品代码不直接读系统时间，测试也不必）。
    crate::errors::classify_response(
        Vendor::Generic,
        WireProtocol::OpenAiChatCompletions,
        (status, &ResponseHeaders::default()),
        body,
        SystemTime::UNIX_EPOCH,
    )
}

fn from_stream(value: &Value) -> Verdict {
    crate::errors::classify_stream_error(
        Vendor::Generic,
        WireProtocol::OpenAiResponses,
        value,
        &value.to_string(),
    )
}

#[test]
fn statuses_and_codes_map_like_the_anthropic_table() {
    let too_long = failure(
        400,
        r#"{"error":{"message":"This model's maximum context length is 8192 tokens","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );
    assert!(matches!(too_long.error, ModelError::PromptTooLong { .. }));
    assert_eq!(too_long.class, Class::ContextOverflow);
    let limited = failure(
        429,
        r#"{"error":{"message":"slow down","type":"requests"}}"#,
    );
    assert!(matches!(limited.error, ModelError::RateLimited { .. }));
    assert_eq!(limited.class, Class::RateLimited);
    let busy = failure(
        503,
        r#"{"error":{"message":"busy","code":"server_is_overloaded"}}"#,
    );
    assert!(matches!(busy.error, ModelError::Overloaded { .. }));
    assert_eq!(busy.class, Class::Overloaded);
    let auth = failure(401, r#"{"error":{"message":"bad key"}}"#);
    assert!(
        matches!(auth.error, ModelError::Authentication { ref message } if message == "bad key")
    );
    assert_eq!(auth.class, Class::AuthRejected);
    let bad = failure(404, "not json");
    assert!(
        matches!(bad.error, ModelError::InvalidRequest { ref message } if message == "not json")
    );
    assert_eq!(bad.class, Class::AccessDenied);
    let server = failure(500, "");
    assert!(
        matches!(server.error, ModelError::Server { status: 500, ref message } if message == "HTTP 500")
    );
    assert_eq!(server.class, Class::ServerError);
}

/// 修改（provider-errors.md D4 暂定）：流里的 `insufficient_quota` 以前按 429 算、会重试十次；响应头之前的 429 同样。
/// 现在是配额用尽，不重试。
#[test]
fn quota_codes_are_quota_exhausted_in_both_forms() {
    let error = from_stream(&json!({"error": {"message": "quota", "code": "insufficient_quota"}}));
    assert_eq!(error.class, Class::QuotaExhausted);
    assert_eq!(
        error.error,
        ModelError::QuotaExhausted {
            message: "quota".to_owned()
        }
    );
    let head = failure(
        429,
        r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#,
    );
    assert_eq!(head.class, Class::QuotaExhausted);
    let kimi = failure(
        429,
        r#"{"error":{"message":"quota","type":"exceeded_current_quota_error"}}"#,
    );
    assert_eq!(kimi.class, Class::QuotaExhausted);
    // 限流码照旧是限流。
    let limited =
        from_stream(&json!({"error": {"message": "slow", "code": "rate_limit_exceeded"}}));
    assert_eq!(limited.class, Class::RateLimited);
}

#[test]
fn stream_errors_use_the_error_type() {
    let error = from_stream(&json!({"type": "error", "message": "boom"}));
    assert!(matches!(
        error.error,
        ModelError::Server { status: 500, .. }
    ));
    assert_eq!(error.class, Class::ServerError);
    let overloaded =
        from_stream(&json!({"error": {"message": "busy", "code": "server_is_overloaded"}}));
    assert_eq!(overloaded.class, Class::Overloaded);
}

/// 内容拦截的错误码（不是结束原因）：终止并显示原因（已定）。
#[test]
fn a_content_filter_error_is_content_blocked() {
    let blocked = failure(
        400,
        r#"{"error":{"message":"The response was filtered","type":"invalid_request_error","code":"content_filter"}}"#,
    );
    assert_eq!(blocked.class, Class::ContentBlocked);
    assert_eq!(
        blocked.error,
        ModelError::ContentBlocked {
            message: "The response was filtered".to_owned()
        }
    );
}

/// 与 Anthropic 一侧共用的词表（修复线 F108，model.md §5.1.1）：别家的说法与错误码也是上下文超限。
#[test]
fn the_shared_overflow_vocabulary_applies() {
    for body in [
        json!({"error": {"message": "The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).", "code": 400}}),
        json!({"error": {"message": "Range of input length should be [1, 129024]", "code": "InvalidParameter"}}),
        json!({"error": {"message": "too many tokens", "code": "context_window_exceeded"}}),
        json!({"error": {"message": "This model's maximum context length is 1048576 tokens. However, you requested 1055008 tokens (671008 in the messages, 384000 in the completion).", "type": "invalid_request_error"}}),
    ] {
        let got = failure(400, &body.to_string());
        assert!(
            matches!(got.error, ModelError::PromptTooLong { .. }),
            "{body}: {:?}",
            got.error
        );
    }
}
