//! http hook 的语义部分（spec §5.2）。

use super::*;
use crate::test_support::ScriptedHttp;

fn hook(url: &str) -> HttpHook {
    HttpHook {
        url: url.to_owned(),
        headers: vec![
            ("Authorization".to_owned(), "Bearer ${TOKEN}".to_owned()),
            ("X-Other".to_owned(), "$SECRET-$TOKEN\r\n".to_owned()),
        ],
        allowed_env_vars: vec!["TOKEN".to_owned(), "SECRET".to_owned()],
        timeout: None,
        condition: None,
    }
}

fn env() -> Vec<(String, String)> {
    vec![
        ("TOKEN".to_owned(), "t0k".to_owned()),
        ("SECRET".to_owned(), "s3c".to_owned()),
    ]
}

#[test]
fn interpolation_only_resolves_allowed_names() {
    let allowed = vec!["A".to_owned()];
    let env = vec![
        ("A".to_owned(), "1".to_owned()),
        ("B".to_owned(), "2".to_owned()),
    ];
    assert_eq!(
        interpolate("x $A ${A} $B ${B} $a ${ x", &allowed, &env),
        "x 1 1   $a ${ x"
    );
    assert_eq!(interpolate("v\r\n\0", &allowed, &env), "v");
}

#[tokio::test]
async fn posts_with_headers_and_policy() {
    let port = ScriptedHttp::answering(200, "{}");
    let env = env();
    let policy_vars = vec!["TOKEN".to_owned()];
    let policy = HttpPolicy {
        allowed_urls: None,
        allowed_env_vars: Some(&policy_vars),
        env: &env,
    };
    let raw = run(
        Some(&port),
        &hook("https://hooks.example.com/h"),
        "{\"a\":1}",
        9_000,
        &policy,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        raw,
        Raw::Http {
            status: 200,
            body: "{}".to_owned()
        }
    );
    let request = port.requests.lock().unwrap().remove(0);
    assert_eq!(request.body, "{\"a\":1}");
    assert_eq!(request.timeout_ms, 9_000);
    assert_eq!(
        request.headers,
        vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("Authorization".to_owned(), "Bearer t0k".to_owned()),
            // SECRET 不在策略允许表里。
            ("X-Other".to_owned(), "-t0k".to_owned()),
        ]
    );
}

/// 配置的 `Content-Type`（任何大小写）盖掉缺省值，只剩一条（F58：缺省值的名字改成小写后按 HTTP 不分大小写合并，
/// 线上与以前相同）。
#[tokio::test]
async fn a_configured_content_type_replaces_the_default() {
    let port = ScriptedHttp::answering(200, "{}");
    let env = env();
    let policy = HttpPolicy {
        allowed_urls: None,
        allowed_env_vars: None,
        env: &env,
    };
    let mut configured = hook("https://hooks.example.com/h");
    configured.headers = vec![("Content-Type".to_owned(), "text/plain".to_owned())];
    run(
        Some(&port),
        &configured,
        "{}",
        1,
        &policy,
        CancellationToken::new(),
    )
    .await;
    let request = port.requests.lock().unwrap().remove(0);
    assert_eq!(
        request.headers,
        vec![("content-type".to_owned(), "text/plain".to_owned())]
    );
}

#[tokio::test]
async fn url_allowlist_and_missing_port() {
    let env = env();
    let patterns = vec!["https://hooks.example.com/*".to_owned()];
    let policy = HttpPolicy {
        allowed_urls: Some(&patterns),
        allowed_env_vars: None,
        env: &env,
    };
    let port = ScriptedHttp::answering(200, "");
    let blocked = run(
        Some(&port),
        &hook("https://other.example.com/h"),
        "{}",
        1,
        &policy,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        blocked,
        Raw::Failed("HTTP hook blocked: https://other.example.com/h does not match any pattern in allowedHttpHookUrls".to_owned())
    );
    assert!(url_matches(
        "https://hooks.example.com/a/b",
        "https://hooks.example.com/*"
    ));
    assert!(!url_matches(
        "https://hooks.example.com.evil/a",
        "https://hooks.example.com/*"
    ));
    let none = run(
        None,
        &hook("https://hooks.example.com/h"),
        "{}",
        1,
        &policy,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(none, Raw::Failed("HTTP hooks are not available".to_owned()));
}
