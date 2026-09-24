//! Request header precedence and gateway routing (Node `model-execution.ts`,
//! `runner-options.ts`, `official-coding-plan-gateway.ts`).
use super::*;
use crate::contract::RequestKind;
use crate::network_status::Attempt;
use serde_json::json;
use zcode_cli_net::{NetworkPolicy, RuntimeEnv};

fn model(api: &str, base: &str, headers: &[(&str, &str)]) -> HttpModel {
    let mut config: ModelConfig = serde_json::from_value(json!({
        "providerId": "p", "modelId": "m", "reasoningLevel": "none",
        "baseUrl": base, "apiType": api,
    }))
    .unwrap();
    config.headers = headers
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    let env = RuntimeEnv::from_vars(vec![("ZCODE_APP_VERSION".into(), "2.0.0".into())]);
    let egress = Egress::new(
        Arc::new(env),
        &NetworkPolicy::default(),
        std::path::Path::new("/nonexistent"),
        "electron",
    )
    .unwrap();
    HttpModel::new(config, Arc::new(egress))
}

fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

fn origin() -> RequestOrigin {
    RequestOrigin {
        kind: RequestKind::Main,
        session_id: Some("sess_abc".into()),
        trace_id: "trace-1".into(),
        query_id: Some("query_q1".into()),
        query_source: "main_turn",
        stream_recovery: None,
    }
}

#[test]
fn provider_and_request_auth_headers_override_identity_but_not_attribution() {
    let model = model(
        "openai-chat-completions",
        "https://openrouter.ai/api/v1",
        &[
            ("x-title", "Provider Title"),
            ("X-ZCode-Trace-Id", "forged"),
        ],
    );
    let auth =
        json!({"requestAuth":{"headers":{"X-TITLE":"Auth Title","authorization":"Token t"}}});
    let headers = model
        .headers(Some("key"), &auth, &origin(), &mut Attempt::new(1))
        .unwrap();
    assert_eq!(header(&headers, "x-title"), Some("Auth Title"));
    assert_eq!(header(&headers, "authorization"), Some("Token t"));
    assert_eq!(header(&headers, "user-agent"), Some("ZCode/2.0.0"));
    assert_eq!(header(&headers, "X-OpenRouter-Title"), Some("ZCode"));
    assert_eq!(header(&headers, "x-zcode-trace-id"), Some("trace-1"));
    assert_eq!(header(&headers, "x-zcode-session-type"), Some("main"));
    assert_eq!(header(&headers, "x-query-id"), Some("q1"));
    assert_eq!(header(&headers, "x-session-id"), Some("abc"));
    let names: Vec<String> = headers.iter().map(|(n, _)| n.to_lowercase()).collect();
    let unique: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(names.len(), unique.len(), "{names:?}");
    // 每次尝试都有新的 x-request-id。
    let again = model
        .headers(Some("key"), &auth, &origin(), &mut Attempt::new(1))
        .unwrap();
    assert_ne!(
        header(&headers, "x-request-id"),
        header(&again, "x-request-id")
    );
}

#[test]
fn anthropic_adds_bearer_only_without_explicit_authorization() {
    let plain = model("anthropic-messages", "https://example.invalid", &[]);
    let headers = plain
        .headers(Some("k"), &Value::Null, &origin(), &mut Attempt::new(1))
        .unwrap();
    assert_eq!(header(&headers, "x-api-key"), Some("k"));
    assert_eq!(header(&headers, "authorization"), Some("Bearer k"));
    assert_eq!(header(&headers, "anthropic-version"), Some("2023-06-01"));
    let explicit = model(
        "anthropic-messages",
        "https://example.invalid",
        &[("Authorization", "Custom c")],
    );
    let headers = explicit
        .headers(Some("k"), &Value::Null, &origin(), &mut Attempt::new(1))
        .unwrap();
    assert_eq!(header(&headers, "authorization"), Some("Custom c"));
}

#[test]
fn official_coding_plan_endpoints_go_through_the_gateway_without_host() {
    let model = model(
        "anthropic-messages",
        "https://api.z.ai/api/anthropic",
        &[("Host", "api.z.ai")],
    );
    assert_eq!(
        model.url,
        "https://zcode.z.ai/api/v1/ultra-zai/anthropic/v1/messages"
    );
    let headers = model
        .headers(None, &Value::Null, &origin(), &mut Attempt::new(1))
        .unwrap();
    assert_eq!(header(&headers, "host"), None);
    let direct = self::model("anthropic-messages", "https://example.invalid", &[]);
    assert_eq!(direct.url, "https://example.invalid/v1/messages");
}

#[test]
fn non_string_request_auth_headers_fail_authentication() {
    let model = model("openai-responses", "https://example.invalid", &[]);
    let auth = json!({"requestAuth":{"headers":{"x":1}}});
    let failure = model
        .headers(None, &auth, &origin(), &mut Attempt::new(1))
        .unwrap_err();
    assert_eq!(failure.reason, "auth_failed");
}
