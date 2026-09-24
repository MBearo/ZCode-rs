//! Parity with Node; fixtures come from `scripts/zcode-cli-rust-web-fixtures.mjs`.
use super::*;
use base64::Engine as _;
use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../../fixtures/web.json")).unwrap()
}

fn outcome<T: ToString>(result: Result<T, WebError>) -> Value {
    match result {
        Ok(value) => serde_json::json!({"ok": value.to_string()}),
        Err(error) => serde_json::json!({"error": {"code": error.code, "message": error.message}}),
    }
}

fn expected(case: &Value) -> Value {
    match case.get("ok") {
        Some(ok) => {
            serde_json::json!({"ok": ok.as_str().map_or_else(|| ok.to_string(), str::to_owned)})
        }
        None => serde_json::json!({"error": case["error"]}),
    }
}

#[test]
fn urls_normalize_like_node() {
    for case in fixtures()["normalize"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        assert_eq!(outcome(url::normalize(input)), expected(case), "{input}");
    }
}

#[test]
fn redirects_follow_node_policy() {
    for case in fixtures()["redirects"].as_array().unwrap() {
        let from = ::url::Url::parse(case["from"].as_str().unwrap()).unwrap();
        let next = url::resolve_redirect(case["location"].as_str().unwrap(), &from).unwrap();
        assert_eq!(next.as_str(), case["next"], "{case}");
        assert_eq!(url::redact_credentials(&next), case["redacted"], "{case}");
        assert_eq!(
            url::permitted_redirect(&from, &next),
            case["permitted"],
            "{case}"
        );
    }
    let base = ::url::Url::parse("https://example.com/").unwrap();
    assert_eq!(
        url::resolve_redirect("https://[x/", &base)
            .unwrap_err()
            .message,
        "Redirect Location is not a valid URL: https://[x/"
    );
}

#[test]
fn literal_egress_matches_ipaddr_js() {
    for case in fixtures()["egress"].as_array().unwrap() {
        let target = ::url::Url::parse(case["url"].as_str().unwrap()).unwrap();
        let result = egress::literal_guard(&target).map(|()| "true");
        assert_eq!(outcome(result), expected(case), "{case}");
    }
}

#[test]
fn readable_content_matches_node() {
    for case in fixtures()["extract"].as_array().unwrap() {
        let body = base64::engine::general_purpose::STANDARD
            .decode(case["body"].as_str().unwrap())
            .unwrap();
        let result = html::extract_readable(&body, case["contentType"].as_str().unwrap());
        assert_eq!(outcome(result), expected(case), "{case}");
    }
    for case in fixtures()["truncate"].as_array().unwrap() {
        let (content, truncated) =
            html::truncate_for_model(&"y".repeat(case["length"].as_u64().unwrap() as usize));
        assert_eq!(truncated, case["truncated"]);
        assert_eq!(content.encode_utf16().count() as u64, case["resultLength"]);
        assert!(content.ends_with(case["tail"].as_str().unwrap()));
    }
}

#[test]
fn prompts_and_texts_match_node() {
    for case in fixtures()["prompts"].as_array().unwrap() {
        let preapproved = case["preapproved"] == true;
        let content = match case["content"].as_str().unwrap() {
            big if big.starts_with("x*") => "x".repeat(big[2..].parse().unwrap()),
            content => content.to_owned(),
        };
        let direct =
            text::direct_markdown(preapproved, case["contentType"].as_str().unwrap(), &content);
        assert_eq!(direct, case["message"].is_null(), "{}", case["contentType"]);
        if direct {
            continue;
        }
        let (sent, truncated) = html::truncate_for_model(&content);
        assert_eq!(truncated, case["result"]["truncated"]);
        let message = text::processing_prompt(&sent, case["prompt"].as_str().unwrap(), preapproved);
        match &case["message"] {
            Value::String(expected) => assert_eq!(&message, expected),
            object => {
                assert_eq!(message.encode_utf16().count() as u64, object["length"]);
                assert!(message.starts_with(object["head"].as_str().unwrap()));
                assert!(message.ends_with(object["tail"].as_str().unwrap()));
            }
        }
    }
    assert_eq!(text::status_text(418, ""), "I'm a Teapot");
    assert_eq!(text::status_text(299, " "), "Unknown Status");
    assert_eq!(text::status_text(404, " Gone "), "Gone");
    assert_eq!(text::retry_after(Some("120")), Some("120"));
    assert_eq!(text::retry_after(Some("1234567")), None);
    assert_eq!(
        text::http_error(503, "Service Unavailable", Some("30")),
        "The server returned HTTP 503 Service Unavailable.\nRetry-After: 30\n\nThe response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch."
    );
}
