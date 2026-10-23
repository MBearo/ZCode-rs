//! 两套直连规则（docs/specs/rust-cli/config.md §2.4）。期望值按原实现逐条推出：Node `http-config.ts` 的
//! `shouldBypassProxy` 与标准的 `no_proxy` 规则。
use super::*;

fn node(rules: &str, url: &str) -> bool {
    NoProxy::Node(rules.to_owned()).bypasses(&Url::parse(url).unwrap())
}

fn standard(rules: &str, url: &str) -> bool {
    NoProxy::Standard(rules.to_owned()).bypasses(&Url::parse(url).unwrap())
}

#[test]
fn no_rules_bypass_nothing() {
    assert!(!NoProxy::None.bypasses(&Url::parse("https://localhost").unwrap()));
    assert!(!node("", "https://localhost"));
    assert!(!standard("", "https://localhost"));
}

#[test]
fn node_hosts_match_themselves_and_their_subdomains() {
    for pattern in [
        "example.com",
        ".example.com",
        "*.example.com",
        " EXAMPLE.com ",
    ] {
        assert!(node(pattern, "https://example.com/v1"), "{pattern}");
        assert!(node(pattern, "https://api.Example.com/v1"), "{pattern}");
        assert!(!node(pattern, "https://notexample.com/v1"), "{pattern}");
    }
    // 末尾的 `.` 两边都去掉。
    assert!(node("example.com.", "https://api.example.com./v1"));
    // IP 按字面比较（同样按“相同或以 `.x` 结尾”），没有 CIDR。
    assert!(node("127.0.0.1", "http://127.0.0.1:8080"));
    assert!(node("0.0.1", "http://127.0.0.1:8080"));
    assert!(!node("127.0.0.0/8", "http://127.0.0.1:8080"));
}

#[test]
fn node_splits_on_commas_only() {
    assert!(node("a.test, ,example.com", "https://example.com"));
    // 空白不是分隔符：整项是一个主机。
    assert!(!node("a.test example.com", "https://example.com"));
}

#[test]
fn node_star_bypasses_everything_even_with_a_port() {
    assert!(node("a.test,*", "https://example.com"));
    assert!(node("*:8080", "https://example.com"));
}

#[test]
fn node_ports_must_match_when_given() {
    assert!(node("example.com:443", "https://example.com/v1"));
    assert!(!node("example.com:8443", "https://example.com/v1"));
    assert!(node("example.com:80", "http://example.com/"));
    assert!(node("example.com:8080", "http://api.example.com:8080/"));
    // `主机:` 的空端口不限制端口。
    assert!(node("example.com:", "https://example.com:9999/"));
}

#[test]
fn node_tokens_with_a_scheme_use_their_host_and_explicit_port() {
    assert!(node("http://example.com", "https://example.com:9999/"));
    assert!(node(
        "https://example.com:8443",
        "https://example.com:8443/"
    ));
    assert!(!node("https://example.com:8443", "https://example.com/"));
    // URL 的缺省端口不算端口。
    assert!(node("https://example.com:443", "https://example.com:9999/"));
    // 解析不了的项跳过。
    assert!(!node("http://exa mple.com", "https://example.com/"));
}

#[test]
fn node_ipv6_in_brackets_ignores_the_port() {
    assert!(node("[::1]", "http://[::1]:8080/"));
    assert!(node("[::1]:9", "http://[::1]:8080/"));
    // 裸的 IPv6 有多个 `:`，整项是主机。
    assert!(node("::1", "http://[::1]:8080/"));
}

#[test]
fn standard_star_only_as_the_whole_value() {
    assert!(standard("*", "https://example.com"));
    // 列表里的 `*` 不是通配符。
    assert!(!standard("a.test,*", "https://example.com"));
}

#[test]
fn standard_splits_on_commas_and_whitespace() {
    assert!(standard("a.test example.com", "https://example.com"));
    assert!(standard("a.test,\texample.com", "https://example.com"));
    assert!(standard("A.TEST,EXAMPLE.COM", "https://example.com"));
}

#[test]
fn standard_hosts_match_exactly_unless_they_start_with_a_dot() {
    assert!(standard("example.com", "https://example.com"));
    assert!(!standard("example.com", "https://api.example.com"));
    assert!(standard(".example.com", "https://example.com"));
    assert!(standard(".example.com", "https://api.example.com"));
    assert!(!standard(".example.com", "https://notexample.com"));
    // `*.x` 没有特殊含义。
    assert!(!standard("*.example.com", "https://api.example.com"));
    // IP 按字面比较，没有 CIDR。
    assert!(standard("127.0.0.1", "http://127.0.0.1:8080"));
    assert!(!standard("127.0.0.0/8", "http://127.0.0.1:8080"));
    // 本机不默认直连（模型请求同样如此）。
    assert!(!standard("example.com", "http://localhost:8080"));
}

#[test]
fn standard_entries_with_a_colon_match_host_and_port() {
    assert!(standard("example.com:443", "https://example.com"));
    assert!(standard("example.com:80", "http://example.com"));
    assert!(!standard("example.com:443", "http://example.com"));
    assert!(standard("example.com:8080", "http://example.com:8080"));
    // 子域不算。
    assert!(!standard("example.com:443", "https://api.example.com"));
    // IPv6 主机带方括号。
    assert!(standard("[::1]:8080", "http://[::1]:8080"));
    assert!(!standard("::1", "http://[::1]:8080"));
}
