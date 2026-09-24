//! Node `webfetch-url.ts`: input URL normalization and the redirect policy.
use super::{MAX_URL_CHARS, WebError};
use crate::js_string;
use url::Url;

fn invalid(message: impl Into<String>) -> WebError {
    WebError::new("webfetch_invalid_url", message)
}

/// Node `normalizeIpAddressLiteral`: lowercase, one trailing dot and brackets removed.
pub(super) fn literal_host(host: &str) -> String {
    let host = host.to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let host = host.strip_prefix('[').unwrap_or(host);
    host.strip_suffix(']').unwrap_or(host).to_owned()
}

/// ipaddr.js `isValid`: the WHATWG parser already turned IP literals into IP hosts.
fn ip_literal(url: &Url) -> bool {
    matches!(url.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)))
}

/// Node `getBlockedHostReason`; IP literals are left to the egress guard.
fn blocked_host(url: &Url) -> Option<&'static str> {
    let host = literal_host(url.host_str().unwrap_or(""));
    if host.is_empty() {
        return Some("URL must include a hostname");
    }
    if ip_literal(url) {
        return None;
    }
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return Some("WebFetch requires a public hostname");
    }
    (host.split('.').count() < 2).then_some("Invalid URL")
}

/// Node `normalizeWebFetchUrl`: checks in Node's order, then http → https.
pub fn normalize(value: &str) -> Result<Url, WebError> {
    if value.encode_utf16().count() > MAX_URL_CHARS {
        return Err(invalid("URL is too long"));
    }
    let mut url =
        Url::parse(js_string::trim(value)).map_err(|_| invalid(format!("Invalid URL: {value}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebError::new(
            "webfetch_unsupported_protocol",
            "WebFetch only supports http and https URLs",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebError::new(
            "webfetch_credentials_in_url",
            "WebFetch URLs must not include credentials",
        ));
    }
    if url.scheme() == "http" {
        // WHATWG 协议 setter：端口若是新协议的默认端口则清除（http://x:443 → https://x）。
        let port = url.port();
        let _ = url.set_scheme("https");
        if port == Some(443) {
            let _ = url.set_port(None);
        }
    }
    if let Some(message) = blocked_host(&url) {
        return Err(invalid(message));
    }
    Ok(url)
}

/// zod `z.string().url()`: the WHATWG parser accepts the raw value.
pub fn zod_url(value: &str) -> bool {
    Url::parse(value).is_ok()
}

/// Node `resolveRedirectUrl`.
pub fn resolve_redirect(location: &str, current: &Url) -> Result<Url, WebError> {
    current.join(location).map_err(|_| {
        WebError::new(
            "webfetch_unsafe_redirect",
            format!("Redirect Location is not a valid URL: {location}"),
        )
    })
}

fn strip_www(host: &str) -> String {
    let host = host.to_lowercase();
    host.strip_prefix("www.")
        .map_or(host.clone(), str::to_owned)
}

/// Node `isPermittedRedirect`: same scheme, port and host (modulo `www.`),
/// no credentials, public host.
pub fn permitted_redirect(from: &Url, to: &Url) -> bool {
    to.username().is_empty()
        && to.password().is_none()
        && blocked_host(to).is_none()
        && from.scheme() == to.scheme()
        && from.port_or_known_default() == to.port_or_known_default()
        && strip_www(from.host_str().unwrap_or("")) == strip_www(to.host_str().unwrap_or(""))
}

/// Node `redactUrlCredentials`.
pub fn redact_credentials(url: &Url) -> String {
    let mut clone = url.clone();
    let _ = clone.set_username("");
    let _ = clone.set_password(None);
    clone.to_string()
}
