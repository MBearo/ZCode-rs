//! 不走代理的规则（docs/specs/rust-cli/config.md §2.4）。两种写法按来源分开，各自照原实现：
//!
//! - [`NoProxy::Node`]：Node 版 zcode 的 `ZCODE_NO_PROXY`（`apps/zcode-cli/packages/adapters/src/network/
//!   http-config.ts` 的 `shouldBypassProxy`、`parseNoProxyToken`、`matchesNoProxyHost`）。
//! - [`NoProxy::Standard`]：标准的 `no_proxy` / `NO_PROXY`。合并两个变量是入口的事，这里收合并后的值。
//!
//! 目标是 reqwest 交给 `Proxy::custom` 的 URL（`scheme://host[:port]`）。主机取 URL 解析后的主机：小写、
//! punycode，IPv6 带方括号（与 WHATWG `URL.hostname` 相同）；端口是缺省端口时 URL 里没有端口（同 WHATWG）。

use reqwest::Url;
use rust_cli_base::js::string::{is_space, trim};

/// 不走代理的规则。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NoProxy {
    /// 全部走代理。
    #[default]
    None,
    /// `ZCODE_NO_PROXY` 的值（Node 的规则）。
    Node(String),
    /// 合并后的 `no_proxy` / `NO_PROXY`（标准规则）。
    Standard(String),
}

impl NoProxy {
    /// 这个目标是否直连。
    pub(crate) fn bypasses(&self, target: &Url) -> bool {
        match self {
            Self::None => false,
            Self::Node(rules) => node_bypasses(rules, target),
            Self::Standard(rules) => standard_bypasses(rules, target),
        }
    }
}

/// 目标的端口：URL 里没有时 https 是 443，其他是 80（两种规则相同；标准规则另把 `wss` 算作 443）。
fn port(target: &Url) -> String {
    target.port().map_or_else(
        || {
            if matches!(target.scheme(), "https" | "wss") {
                "443".to_owned()
            } else {
                "80".to_owned()
            }
        },
        |port| port.to_string(),
    )
}

/// Node `normalizeNoProxyHost`：去空白、去方括号、去末尾的 `.`、转小写。
fn node_host(value: &str) -> String {
    let value = trim(value);
    let value = value.strip_prefix('[').unwrap_or(value);
    let value = value.strip_suffix(']').unwrap_or(value);
    let value = value.strip_suffix('.').unwrap_or(value);
    value.to_lowercase()
}

/// Node 的一项规则：主机与可选的端口。
struct NodeToken {
    host: String,
    port: Option<String>,
}

/// Node `parseNoProxyToken`。
fn node_token(raw: &str) -> Option<NodeToken> {
    let trimmed = trim(raw).to_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "*" {
        return Some(NodeToken {
            host: "*".to_owned(),
            port: None,
        });
    }
    if trimmed.contains("://") {
        let parsed = Url::parse(&trimmed).ok()?;
        return Some(NodeToken {
            host: node_host(parsed.host_str().unwrap_or("")),
            port: parsed.port().map(|port| port.to_string()),
        });
    }
    // `[…]` 开头：取方括号里的地址，后面的端口不看。没有 `]` 时 JS `slice(1, -1)` 去掉首尾各一个字符。
    if let Some(rest) = trimmed.strip_prefix('[') {
        let inner = match rest.find(']') {
            Some(end) => &rest[..end],
            None => rest
                .char_indices()
                .last()
                .map_or("", |(last, _)| &rest[..last]),
        };
        if !inner.is_empty() {
            return Some(NodeToken {
                host: node_host(inner),
                port: None,
            });
        }
    }
    // 恰好一个 `:` 且不在开头才是 `主机:端口`；其他情况（包括裸的 IPv6）整项是主机。
    match (trimmed.find(':'), trimmed.rfind(':')) {
        (Some(first), Some(last)) if first == last && last > 0 => Some(NodeToken {
            host: node_host(&trimmed[..last]),
            port: Some(trimmed[last + 1..].to_owned()),
        }),
        _ => Some(NodeToken {
            host: node_host(&trimmed),
            port: None,
        }),
    }
}

/// Node `matchesNoProxyHost`：`x`、`.x`、`*.x` 都匹配 `x` 本身与以 `.x` 结尾的主机。
fn node_host_matches(host: &str, pattern: &str) -> bool {
    let suffix = pattern
        .strip_prefix("*.")
        .or_else(|| pattern.strip_prefix('.'))
        .unwrap_or(pattern);
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// Node `shouldBypassProxy`：按逗号分隔，`*` 全部直连，项带端口时端口要相同。
fn node_bypasses(rules: &str, target: &Url) -> bool {
    let host = node_host(target.host_str().unwrap_or(""));
    if host.is_empty() || rules.is_empty() {
        return false;
    }
    let port = port(target);
    for token in rules.split(',').filter_map(node_token) {
        // 先看 `*` 再看端口：`*:8080` 也是全部直连（Node 相同）。
        if token.host == "*" {
            return true;
        }
        if token
            .port
            .as_deref()
            .is_some_and(|wanted| !wanted.is_empty() && wanted != port)
        {
            continue;
        }
        if node_host_matches(&host, &token.host) {
            return true;
        }
    }
    false
}

/// 标准规则：值是 `*` 时全部直连；按逗号与空白分隔；含 `:` 的项与 `主机:端口` 完全相同；
/// `.x` 匹配 `x` 与以 `.x` 结尾的主机；其他项与主机完全相同。
fn standard_bypasses(rules: &str, target: &Url) -> bool {
    if rules.is_empty() {
        return false;
    }
    if rules == "*" {
        return true;
    }
    let host = target.host_str().unwrap_or("").to_lowercase();
    let host_port = format!("{host}:{}", port(target));
    rules
        .split(|c: char| c == ',' || is_space(c))
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| {
            let pattern = pattern.to_lowercase();
            if pattern.contains(':') {
                host_port == pattern
            } else if let Some(bare) = pattern.strip_prefix('.') {
                host == bare || host.ends_with(&pattern)
            } else {
                host == pattern
            }
        })
}

#[cfg(test)]
#[path = "no_proxy_tests.rs"]
mod tests;
