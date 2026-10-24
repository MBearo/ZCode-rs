//! Monitor 输入的细校验与私有地址（tools-more.md §19.3、§19.4；MO1 的原文）。

use serde_json::json;

use super::*;

#[test]
fn both_sources_or_none_fail_with_the_zod_issue() {
    let both =
        json!({"description": "d", "command": "echo x", "ws": {"url": "wss://e.example.com/s"}});
    assert_eq!(
        validation_message(&issues(&both).unwrap()),
        "InputValidationError: [\n  {\n    \"code\": \"custom\",\n    \"path\": [],\n    \"message\": \"exactly one of command or ws\"\n  }\n]"
    );
    assert!(issues(&json!({"description": "d"})).is_some());
    assert!(issues(&json!({"description": "d", "command": "tail -f x"})).is_none());
}

#[test]
fn a_short_deadline_is_too_small() {
    let input = json!({"description": "d", "command": "echo x", "timeout_ms": 500});
    assert_eq!(
        validation_message(&issues(&input).unwrap()),
        "InputValidationError: [\n  {\n    \"origin\": \"number\",\n    \"code\": \"too_small\",\n    \"minimum\": 1000,\n    \"inclusive\": true,\n    \"path\": [\n      \"timeout_ms\"\n    ],\n    \"message\": \"Too small: expected number to be >=1000\"\n  }\n]"
    );
    let long = json!({"description": "d", "command": "echo x", "timeout_ms": 3_600_001});
    let issue = &issues(&long).unwrap()[0];
    assert_eq!(issue["code"], "too_big");
    assert_eq!(issue["message"], "timeout_ms must be \u{2264} 3600000");
}

#[test]
fn urls_protocols_and_controls_are_checked() {
    let bad = |ws: serde_json::Value| issues(&json!({"description": "d", "ws": ws})).unwrap();
    assert_eq!(
        bad(json!({"url": "https://e.example.com"}))[0]["message"],
        "url must be a valid ASCII ws:// or wss:// URL with no userinfo or whitespace"
    );
    assert_eq!(
        bad(json!({"url": "wss://user:pw@e.example.com"}))[0]["path"],
        json!(["ws", "url"])
    );
    assert_eq!(
        bad(json!({"url": "wss://e.example.com", "protocols": ["v1", "v1"]}))[0]["message"],
        "protocols must be unique"
    );
    let issue = &bad(json!({"url": "wss://e.example.com", "protocols": ["a b"]}))[0];
    assert_eq!(issue["path"], json!(["ws", "protocols", 0]));
    assert_eq!(issue["message"], "protocol must be an RFC 6455 token");
    let control = issues(&json!({"description": "d", "command": "echo \u{1b}[2J"})).unwrap();
    assert_eq!(
        control[0]["message"],
        "command contains control characters that would be hidden in the approval dialog"
    );
    assert!(issues(&json!({"description": "d", "command": "a\tb\nc"})).is_none());
}

#[test]
fn private_hosts_follow_the_ranges() {
    assert_eq!(
        private_host("ws://10.0.0.1:9/feed").as_deref(),
        Some("10.0.0.1")
    );
    assert_eq!(
        private_host("ws://192.168.1.2/").as_deref(),
        Some("192.168.1.2")
    );
    assert_eq!(
        private_host("ws://169.254.169.254/").as_deref(),
        Some("169.254.169.254")
    );
    assert_eq!(private_host("ws://127.0.0.1:9/feed"), None);
    assert_eq!(private_host("ws://[::1]/"), None);
    assert_eq!(private_host("ws://[fd00::1]/").as_deref(), Some("fd00::1"));
    assert_eq!(private_host("wss://events.example.com/stream"), None);
}

#[test]
fn the_deadline_defaults_and_is_capped() {
    let input = |timeout: Option<f64>| Input {
        description: "d".to_owned(),
        timeout_ms: timeout,
        command: Some("x".to_owned()),
        ws: None,
    };
    assert_eq!(input(None).timeout(600_000), 300_000);
    assert_eq!(input(Some(3_600_000.0)).timeout(600_000), 600_000);
    assert_eq!(input(Some(60_000.0)).timeout(1_800_000), 60_000);
}
