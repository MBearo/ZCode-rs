//! Per-request proxy selection (Node `adapters/src/network/http-config.ts`).
//! Model and MCP requests only honor explicit ZCode settings; WebFetch may fall
//! back to the shell proxy captured in the tool passthrough.
use crate::{
    child::NetworkPolicy,
    env::{PASSTHROUGH_KEY, read_passthrough},
};
use url::Url;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyResolution {
    pub no_proxy_matched: bool,
    pub source: Option<String>,
    pub proxy: Option<String>,
}

impl ProxyResolution {
    fn via(source: String, proxy: String) -> Self {
        Self {
            no_proxy_matched: false,
            source: Some(source),
            proxy: Some(proxy),
        }
    }

    fn bypass() -> Self {
        Self {
            no_proxy_matched: true,
            ..Self::default()
        }
    }
}

/// Resolved once from the startup policy and environment view.
#[derive(Clone, Debug, Default)]
pub struct ProxyRules {
    no_proxy: Option<String>,
    explicit: Option<(String, String)>,
    captured_no_proxy: Option<String>,
    captured: Option<(String, String)>,
}

const CAPTURED_PROXY_KEYS: [&str; 6] = [
    "https_proxy",
    "HTTPS_PROXY",
    "http_proxy",
    "HTTP_PROXY",
    "all_proxy",
    "ALL_PROXY",
];

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// `/^[a-z][a-z0-9+.-]*:\/\//i`.
pub(crate) fn has_scheme(value: &str) -> bool {
    value.split_once("://").is_some_and(|(scheme, _)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    })
}

/// Node `normalizeProxyUrl`: scheme defaults to http, result is the WHATWG href.
fn normalize_proxy_url(value: Option<&str>) -> Option<String> {
    let value = trimmed(value)?;
    let candidate = if has_scheme(&value) {
        value
    } else {
        format!("http://{value}")
    };
    Url::parse(&candidate).ok().map(String::from)
}

impl ProxyRules {
    pub fn new<'a>(network: &NetworkPolicy, env: impl Fn(&str) -> Option<&'a str>) -> Self {
        let explicit = normalize_proxy_url(network.http_proxy.as_deref())
            .map(|proxy| ("network.httpProxy".to_owned(), proxy))
            .or_else(|| {
                normalize_proxy_url(env("ZCODE_HTTP_PROXY"))
                    .map(|proxy| ("env:ZCODE_HTTP_PROXY".to_owned(), proxy))
            });
        let captured = read_passthrough(env(PASSTHROUGH_KEY));
        Self {
            no_proxy: trimmed(network.no_proxy.as_deref())
                .or_else(|| trimmed(env("ZCODE_NO_PROXY"))),
            explicit,
            captured_no_proxy: trimmed(captured.get("no_proxy").map(String::as_str))
                .or_else(|| trimmed(captured.get("NO_PROXY").map(String::as_str))),
            captured: CAPTURED_PROXY_KEYS.iter().find_map(|key| {
                normalize_proxy_url(captured.get(*key).map(String::as_str))
                    .map(|proxy| (format!("env:{PASSTHROUGH_KEY}.{key}"), proxy))
            }),
        }
    }

    /// Whether any request can be proxied without the WebFetch fallback.
    pub fn has_explicit_proxy(&self) -> bool {
        self.explicit.is_some()
    }

    /// Node `resolveProxyForRequest` (`web_fetch` = `resolveWebFetchProxyForRequest`).
    pub fn resolve(&self, url: &str, web_fetch: bool) -> ProxyResolution {
        let Some(url) = Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
        else {
            return ProxyResolution::default();
        };
        if bypasses(&url, self.no_proxy.as_deref()) {
            return ProxyResolution::bypass();
        }
        if let Some((source, proxy)) = &self.explicit {
            return ProxyResolution::via(source.clone(), proxy.clone());
        }
        if !web_fetch {
            return ProxyResolution::default();
        }
        if bypasses(&url, self.captured_no_proxy.as_deref()) {
            return ProxyResolution::bypass();
        }
        match &self.captured {
            Some((source, proxy)) => ProxyResolution::via(source.clone(), proxy.clone()),
            None => ProxyResolution::default(),
        }
    }
}

/// Node `normalizeNoProxyHost`.
fn normalize_host(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix('[').unwrap_or(value);
    let value = value.strip_suffix(']').unwrap_or(value);
    value.strip_suffix('.').unwrap_or(value).to_lowercase()
}

struct Token {
    host: String,
    port: Option<String>,
}

/// Node `parseNoProxyToken`.
fn parse_token(raw: &str) -> Option<Token> {
    let token = raw.trim().to_lowercase();
    if token.is_empty() {
        return None;
    }
    if token == "*" {
        return Some(Token {
            host: "*".into(),
            port: None,
        });
    }
    if token.contains("://") {
        let parsed = Url::parse(&token).ok()?;
        return Some(Token {
            host: normalize_host(parsed.host_str().unwrap_or("")),
            port: parsed.port().map(|p| p.to_string()),
        });
    }
    if token.starts_with('[') {
        // 与 JS `slice(1, indexOf("]"))` 一致：缺少右括号时 indexOf 为 -1，去掉最后一个字符。
        let end = token.find(']').unwrap_or_else(|| {
            token
                .char_indices()
                .next_back()
                .map_or(token.len(), |(at, _)| at)
        });
        let inner = token.get(1..end.max(1)).unwrap_or("");
        if !inner.is_empty() {
            return Some(Token {
                host: normalize_host(inner),
                port: None,
            });
        }
    }
    match token.rfind(':') {
        Some(separator) if separator > 0 && token.find(':') == Some(separator) => Some(Token {
            host: normalize_host(&token[..separator]),
            port: Some(token[separator + 1..].to_owned()),
        }),
        _ => Some(Token {
            host: normalize_host(&token),
            port: None,
        }),
    }
}

/// Node `matchesNoProxyHost`.
fn matches_host(host: &str, pattern: &str) -> bool {
    let suffix = pattern
        .strip_prefix("*.")
        .or_else(|| pattern.strip_prefix('.'));
    match suffix {
        Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
        None => host == pattern || host.ends_with(&format!(".{pattern}")),
    }
}

/// Node `shouldBypassProxy`.
fn bypasses(url: &Url, no_proxy: Option<&str>) -> bool {
    let host = normalize_host(url.host_str().unwrap_or(""));
    let Some(no_proxy) = no_proxy.filter(|_| !host.is_empty()) else {
        return false;
    };
    let port = url.port().map_or_else(
        || if url.scheme() == "https" { "443" } else { "80" }.to_owned(),
        |p| p.to_string(),
    );
    no_proxy.split(',').filter_map(parse_token).any(|token| {
        token.host == "*"
            || (token
                .port
                .as_deref()
                .is_none_or(|p| p.is_empty() || p == port)
                && matches_host(&host, &token.host))
    })
}
