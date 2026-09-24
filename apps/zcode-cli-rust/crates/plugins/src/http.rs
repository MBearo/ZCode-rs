//! HTTP port of plugin management (implemented by the tools adapter over the
//! WebFetch egress) and the marketplace JSON request (Node
//! `requestMarketplaceJson`). Spec rust-m10-4-plugin-sources §4.3.
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MARKETPLACE_JSON_MAX_BYTES: u64 = 10 * 1024 * 1024;
const MARKETPLACE_JSON_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_REDIRECTS: usize = 5;
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

pub struct Get<'a> {
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub max_bytes: u64,
    pub timeout: Duration,
}

pub struct Response {
    pub status: u16,
    pub status_text: String,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

/// One GET without following redirects. Errors carry the Node HTTP adapter's
/// messages (`fetch failed`, `HTTP request timed out after <ms>ms`, …).
#[async_trait::async_trait]
pub trait Http: Send + Sync {
    async fn get(&self, request: Get<'_>, cancel: &CancellationToken) -> Result<Response>;
}

pub fn is_redirect(status: u16) -> bool {
    REDIRECT_STATUSES.contains(&status)
}

/// `new URL(location, current)`.
pub fn resolve_location(location: &str, current: &str) -> Result<String> {
    let base = url::Url::parse(current).map_err(|_| anyhow!("Invalid URL: {current}"))?;
    let next = base
        .join(location)
        .map_err(|_| anyhow!("Invalid URL: {location}"))?;
    Ok(next.to_string())
}

/// Whether two URLs share an origin (custom headers never cross origins).
pub fn same_origin(left: &str, right: &str) -> bool {
    match (url::Url::parse(left), url::Url::parse(right)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// Node `requestMarketplaceJson`.
pub async fn marketplace_json(
    http: &dyn Http,
    url: &str,
    headers: &[(String, String)],
    cancel: &CancellationToken,
) -> Result<Value> {
    let mut current = url.to_owned();
    let mut current_headers = headers.to_vec();
    for _ in 0..=MAX_REDIRECTS {
        let response = http
            .get(
                Get {
                    url: &current,
                    headers: &current_headers,
                    max_bytes: MARKETPLACE_JSON_MAX_BYTES,
                    timeout: MARKETPLACE_JSON_TIMEOUT,
                },
                cancel,
            )
            .await?;
        if is_redirect(response.status) {
            let Some(location) = response.location else {
                bail!("Marketplace redirect is missing Location header: {current}");
            };
            let next = resolve_location(&location, &current)?;
            // 跨 origin 跳转清理市场自定义 header，避免凭据泄露给 CDN。
            if !same_origin(&next, &current) {
                current_headers.clear();
            }
            current = next;
            continue;
        }
        if !(200..300).contains(&response.status) {
            bail!(
                "Failed to fetch marketplace: {} {}",
                response.status,
                response.status_text
            );
        }
        let text = String::from_utf8_lossy(&response.body);
        return Ok(serde_json::from_str(&text)?);
    }
    bail!("Marketplace fetch exceeded redirect limit: {url}")
}

/// `source.headers` of a URL source as string pairs.
pub fn source_headers(source: &Value) -> Vec<(String, String)> {
    source["headers"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect()
}
