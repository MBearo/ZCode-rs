//! 判决链的小件：`domain:` 规则的主机名（shell-permissions.md §4.2、tools-more.md §4）。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::run;

/// 主机名按 WHATWG URL 取（与 WebFetch 工具解析网址、预批准域名同一个解析器）。F125：以前按字符串切分，下面标“修前”的
/// 几行结论不同——反斜杠在 http(s) 里是路径分隔符，以前把 `@` 之后当主机；IPv6 以前只剩 `[`；国际化域名以前不转
/// punycode；端口不合法、开头有制表符时以前照样切出一个主机。
#[test]
fn rule_hosts_follow_the_url_parser() {
    for (url, host) in [
        ("https://Example.COM/a", "example.com"),
        ("https://user:pw@example.com:8080/x", "example.com"),
        // 修前：evil.com（规则查的主机与实际取的主机不同）。
        (r"https://example.com\@evil.com/", "example.com"),
        // 修前：`[`。
        ("http://[::1]:8080/", "[::1]"),
        // 修前：bücher.example。
        ("https://b\u{fc}cher.example/", "xn--bcher-kva.example"),
        // 修前：`\texample.com`。
        ("https://\texample.com/", "example.com"),
        // 修前：example.com；网址不合法，工具本身也会拒绝。
        ("https://example.com:abc/", ""),
        ("https://example.com?x=1", "example.com"),
        ("https://a@b@example.com/", "example.com"),
        ("https://example.com./", "example.com."),
        ("foo://Example.com/x", "example.com"),
        ("example.com", ""),
    ] {
        assert_eq!(rule_host(url), host, "{url}");
    }
}

/// deny 规则看的就是工具实际要取的主机。
#[test]
fn a_backslash_does_not_move_the_host_past_a_deny_rule() {
    let check = run(
        PermissionMode::Default,
        &[(RuleBehavior::Deny, "WebFetch(domain:example.com)")],
        |inputs| web_fetch(r"https://example.com\@evil.com/", inputs),
    );
    assert_eq!(
        check,
        Check::deny("WebFetch denied access to domain:example.com.")
    );
}
