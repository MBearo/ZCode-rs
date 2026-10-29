//! 智谱的码表与结束原因（provider-errors.md §5.3）：每个码一行，到达形式（响应头之前、流里、不带事件名的块）各测一组。

use std::time::SystemTime;

use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::WireProtocol;
use rust_cli_net::ResponseHeaders;
use serde_json::{Value, json};

use crate::errors::{Class, Finish, Vendor, Verdict};

fn response(vendor: Vendor, protocol: WireProtocol, status: u16, body: &Value) -> Verdict {
    crate::errors::classify_response(
        vendor,
        protocol,
        (status, &ResponseHeaders::default()),
        &body.to_string(),
        SystemTime::UNIX_EPOCH,
    )
}

fn stream(value: &Value) -> Verdict {
    crate::errors::classify_stream_error(
        Vendor::Zhipu,
        WireProtocol::AnthropicMessages,
        value,
        &value.to_string(),
    )
}

/// 码 → 中性分类（§5.3 的两张表，一个码一行）。
const TABLE: [(&str, Class); 43] = [
    ("500", Class::ServerError),
    ("1008", Class::InvalidRequest),
    ("1113", Class::QuotaExhausted),
    ("1120", Class::ServerError),
    ("1210", Class::InvalidRequest),
    ("1211", Class::AccessDenied),
    ("1213", Class::InvalidRequest),
    ("1214", Class::AccessDenied),
    ("1230", Class::ServerError),
    ("1234", Class::Network),
    ("1261", Class::ContextOverflow),
    ("1301", Class::ContentBlocked),
    ("1302", Class::RateLimited),
    ("1303", Class::RateLimited),
    ("1304", Class::QuotaExhausted),
    ("1305", Class::RateLimited),
    ("1308", Class::QuotaExhausted),
    ("1309", Class::QuotaExhausted),
    ("1310", Class::QuotaExhausted),
    ("1311", Class::AccessDenied),
    ("1312", Class::Overloaded),
    ("1313", Class::QuotaExhausted),
    ("1314", Class::QuotaExhausted),
    ("1315", Class::QuotaExhausted),
    ("1316", Class::QuotaExhausted),
    ("1317", Class::QuotaExhausted),
    ("1318", Class::QuotaExhausted),
    ("1319", Class::QuotaExhausted),
    ("1320", Class::QuotaExhausted),
    ("1321", Class::QuotaExhausted),
    ("2056", Class::QuotaExhausted),
    ("20097", Class::QuotaExhausted),
    ("1005", Class::QuotaExhausted),
    ("1006", Class::AuthRejected),
    ("2007", Class::ServerError),
    ("3001", Class::InvalidRequest),
    ("3002", Class::RateLimited),
    ("3006", Class::AccessDenied),
    ("3007", Class::AccessDenied),
    ("3008", Class::CapacityLimited),
    ("3009", Class::CapacityLimited),
    ("3010", Class::CapacityLimited),
    // 表外的码在流里：参数错误，不重试。
    ("1999", Class::InvalidRequest),
];

/// 码在表里就按表，不看 HTTP 状态（429 + 配额码不重试，401 + 限流码按限流）；两种协议的正文形状、字符串或数字的码都认。
#[test]
fn table_codes_win_over_the_http_status() {
    for (code, class) in TABLE.iter().filter(|(code, _)| *code != "1999") {
        for protocol in [
            WireProtocol::AnthropicMessages,
            WireProtocol::OpenAiChatCompletions,
        ] {
            for status in [400, 401, 429, 500] {
                let body = json!({"error": {"code": code, "message": "原文"}});
                let got = response(Vendor::Zhipu, protocol, status, &body);
                assert_eq!(got.class, *class, "{code} on {status} ({protocol:?})");
            }
        }
        let numeric: u64 = code.parse().unwrap();
        let got = response(
            Vendor::Zhipu,
            WireProtocol::AnthropicMessages,
            429,
            &json!({"type": "error", "error": {"type": "rate_limit_error", "code": numeric, "message": "原文"}}),
        );
        assert_eq!(got.class, *class, "numeric {code}");
    }
}

/// 不是智谱的端点：同样的正文按协议与状态（智谱的码表只在智谱的端点用，D2）。
#[test]
fn other_endpoints_do_not_read_the_codes() {
    let body = json!({"error": {"code": "1308", "message": "已达到套餐用量上限"}});
    let got = response(Vendor::Generic, WireProtocol::AnthropicMessages, 429, &body);
    assert_eq!(got.class, Class::RateLimited);
    let zhipu = response(Vendor::Zhipu, WireProtocol::AnthropicMessages, 429, &body);
    assert_eq!(zhipu.class, Class::QuotaExhausted);
    assert_eq!(
        zhipu.error,
        ModelError::QuotaExhausted {
            message: "已达到套餐用量上限".to_owned()
        }
    );
}

/// 表外的码有 HTTP 状态时交给协议层；不是纯数字的码不算业务码。
#[test]
fn unknown_codes_with_a_status_fall_through() {
    let unknown = response(
        Vendor::Zhipu,
        WireProtocol::OpenAiChatCompletions,
        503,
        &json!({"error": {"code": "1999", "message": "busy"}}),
    );
    assert_eq!(unknown.class, Class::ServerError);
    let named = response(
        Vendor::Zhipu,
        WireProtocol::AnthropicMessages,
        529,
        &json!({"type": "error", "error": {"type": "overloaded_error", "code": "overloaded", "message": "x"}}),
    );
    assert_eq!(named.class, Class::Overloaded);
}

/// 码的来源：`providerCode`、`error_code`、`code`（`error` 下或顶层），消息开头的 `[1302]`。
#[test]
fn codes_come_from_the_known_fields() {
    for body in [
        json!({"error": {"providerCode": "1302", "message": "x"}}),
        json!({"error": {"error_code": 1302, "message": "x"}}),
        json!({"code": 1302, "msg": "x"}),
        json!({"error": {"message": "[1302][并发数过高] 请稍后再试"}}),
    ] {
        let got = response(Vendor::Zhipu, WireProtocol::AnthropicMessages, 400, &body);
        assert_eq!(got.class, Class::RateLimited, "{body}");
    }
    let top = response(
        Vendor::Zhipu,
        WireProtocol::AnthropicMessages,
        400,
        &json!({"code": "1113", "msg": "余额不足"}),
    );
    assert_eq!(
        top.error,
        ModelError::QuotaExhausted {
            message: "余额不足".to_owned()
        }
    );
}

/// 流里的错误块：码在表里按表；表外的码是参数错误（不重试）；没有码时交给协议层。错误形状：服务端错误没有状态时按 500，
/// 无权限时是参数错误的形状。
#[test]
fn stream_errors_follow_the_table() {
    for (code, class) in TABLE {
        let got = stream(&json!({"error": {"code": code, "message": "原文"}}));
        assert_eq!(got.class, class, "{code}");
    }
    assert_eq!(
        stream(&json!({"error": {"code": "1230", "message": "x"}})).error,
        ModelError::Server {
            status: 500,
            message: "x".to_owned()
        }
    );
    assert!(matches!(
        stream(&json!({"error": {"code": "1311", "message": "x"}})).error,
        ModelError::InvalidRequest { .. }
    ));
    assert!(matches!(
        stream(&json!({"error": {"code": "3008", "message": "x"}})).error,
        ModelError::RateLimited { .. }
    ));
    let protocol =
        stream(&json!({"type": "error", "error": {"type": "overloaded_error", "message": "x"}}));
    assert_eq!(protocol.class, Class::Overloaded);
}

/// 不带事件名的块：`success: false` 或表里的码是错误；带非错误码的普通块不是。
#[test]
fn unnamed_frames_with_codes_are_errors() {
    let frame = |value: Value| {
        crate::errors::unnamed_frame(
            Vendor::Zhipu,
            WireProtocol::AnthropicMessages,
            &value,
            &value.to_string(),
        )
    };
    assert_eq!(
        frame(json!({"code": 1302, "msg": "并发数过高"}))
            .unwrap()
            .class,
        Class::RateLimited
    );
    assert_eq!(
        frame(json!({"success": false, "msg": "出错了"}))
            .unwrap()
            .class,
        Class::InvalidRequest
    );
    assert!(frame(json!({"code": 200, "msg": "ok"})).is_none());
    assert!(frame(json!({"type": "message_delta"})).is_none());
}

/// 结束原因：`sensitive` 是内容拦截，`network_error` 是流内的网络失败；别的值照通用表。
#[test]
fn finish_reasons_of_the_vendor() {
    assert_eq!(
        crate::errors::classify_finish(Vendor::Zhipu, "sensitive"),
        Finish::Stop(StopReason::Refusal)
    );
    let Finish::Failed(failed) = crate::errors::classify_finish(Vendor::Zhipu, "network_error")
    else {
        panic!("network_error is a failure");
    };
    assert_eq!(failed.class, Class::Network);
    assert!(matches!(failed.error, ModelError::Network { .. }));
    assert_eq!(
        crate::errors::classify_finish(Vendor::Zhipu, "model_context_window_exceeded"),
        Finish::Stop(StopReason::ModelContextWindowExceeded)
    );
    // 别的端点不认这两个值（以前两个都是 `Unknown`）。
    assert_eq!(
        crate::errors::classify_finish(Vendor::Generic, "sensitive"),
        Finish::Stop(StopReason::Unknown)
    );
}
