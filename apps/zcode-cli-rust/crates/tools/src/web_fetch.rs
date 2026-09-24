//! WebFetch's network side (Node `webfetch-network.ts`, `webfetch-cache.ts`):
//! the redirect loop behind the literal-IP guard, the process-wide cache and
//! the raw artifact of large pages. Spec rust-m5-tools §3.
use crate::domain::web::{self, FetchRequest, Fetched, Hop, Page, WebError};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use zcode_cli_net::{Egress, Purpose};

struct Entry {
    page: Page,
    expires: Instant,
    size: usize,
    seq: u64,
}

/// Node's module-level cache: keyed by the raw input URL, 15 minutes, 50 MiB,
/// oldest-used evicted first; only readable pages are kept.
#[derive(Default)]
struct Cache {
    entries: HashMap<String, Entry>,
    order: BTreeMap<u64, String>,
    seq: u64,
    bytes: usize,
}

impl Cache {
    fn remove(&mut self, key: &str) -> Option<Entry> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&entry.seq);
        self.bytes -= entry.size;
        Some(entry)
    }

    fn get(&mut self, key: &str, now: Instant) -> Option<Page> {
        let mut entry = self.remove(key)?;
        if entry.expires <= now {
            return None;
        }
        let page = entry.page.clone();
        self.seq += 1;
        entry.seq = self.seq;
        self.bytes += entry.size;
        self.order.insert(entry.seq, key.to_owned());
        self.entries.insert(key.to_owned(), entry);
        Some(page)
    }

    fn put(&mut self, key: &str, page: Page, now: Instant) {
        let size = page.content.len();
        if size > web::CACHE_MAX_BYTES {
            return;
        }
        self.remove(key);
        self.seq += 1;
        let expires = now + Duration::from_millis(web::CACHE_TTL_MS);
        self.order.insert(self.seq, key.to_owned());
        let seq = self.seq;
        self.entries.insert(
            key.to_owned(),
            Entry {
                page,
                expires,
                size,
                seq,
            },
        );
        self.bytes += size;
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.expires <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            self.remove(&key);
        }
        while self.bytes > web::CACHE_MAX_BYTES {
            let Some(oldest) = self.order.values().next().cloned() else {
                break;
            };
            self.remove(&oldest);
        }
    }
}

pub(super) struct WebFetcher {
    egress: Arc<Egress>,
    cache: Mutex<Cache>,
}

struct Response {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

fn header<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn failed(message: impl Into<String>) -> anyhow::Error {
    WebError::new("webfetch_fetch_failed", message).into()
}

/// Node `formatRequestError` for the transport errors reqwest reports.
fn request_error(error: &reqwest::Error, url: &url::Url) -> anyhow::Error {
    let port = url.port_or_known_default().unwrap_or(443);
    let host = url.host_str().unwrap_or("");
    if error.is_timeout() && error.is_connect() {
        return failed(format!("Connect Timeout Error ({port}, timeout 10000ms)"));
    }
    if error.is_timeout() {
        return failed(format!(
            "HTTP request timed out after {}ms",
            web::TIMEOUT_MS
        ));
    }
    let mut deepest: &dyn std::error::Error = error;
    let mut kind = None;
    while let Some(source) = deepest.source() {
        if let Some(io) = source.downcast_ref::<std::io::Error>() {
            kind = Some(io.kind());
        }
        deepest = source;
    }
    let text = deepest.to_string();
    // undici 的错误文本无法逐字复现：常见类别映射成 Node 的形态（spec 差异）。
    let message = match kind {
        Some(std::io::ErrorKind::ConnectionRefused) => {
            format!("fetch failed: connect ECONNREFUSED {host}:{port}")
        }
        Some(std::io::ErrorKind::ConnectionReset) => "fetch failed: read ECONNRESET".to_owned(),
        _ if text.contains("lookup address") || text.contains("dns error") => {
            format!("fetch failed: getaddrinfo ENOTFOUND {host}")
        }
        _ => format!("fetch failed: {text}"),
    };
    failed(message)
}

fn too_large(message: String) -> anyhow::Error {
    WebError::new("webfetch_response_too_large", message).into()
}

impl WebFetcher {
    pub(super) fn new(egress: Arc<Egress>) -> Self {
        Self {
            egress,
            cache: Mutex::default(),
        }
    }

    /// One GET with Node's headers; the body is read (bounded) even for 3xx/4xx.
    async fn get(
        &self,
        url: &url::Url,
        trace: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        let client = self.egress.client(Purpose::WebFetch).await?;
        let mut request = client
            .get(url.as_str())
            .header("user-agent", web::USER_AGENT)
            .header("accept", web::ACCEPT)
            .header("accept-language", "*")
            .header("sec-fetch-mode", "cors")
            .timeout(Duration::from_millis(web::TIMEOUT_MS));
        if let Some(trace) = trace {
            request = request.header("x-zcode-trace-id", trace);
        }
        let exchange = async {
            let mut response = request.send().await.map_err(|e| request_error(&e, url))?;
            let max = web::MAX_RESPONSE_BYTES;
            if let Some(length) = response.content_length().filter(|n| *n > max) {
                return Err(too_large(format!(
                    "HTTP response is too large: content-length={length}, max={max}"
                )));
            }
            let mut body = vec![];
            while let Some(chunk) = response.chunk().await.map_err(|e| request_error(&e, url))? {
                if (body.len() + chunk.len()) as u64 > max {
                    return Err(too_large(format!(
                        "HTTP response is too large: bytes>{max}"
                    )));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Response {
                status: response.status().as_u16(),
                headers: response.headers().clone(),
                body,
            })
        };
        tokio::select! {
            _ = cancel.cancelled() => bail!("Cancelled"),
            result = exchange => result,
        }
    }

    /// Node `fetchAndExtractContent` behind the cache (URL checks run first).
    pub(super) async fn fetch(
        &self,
        request: &FetchRequest,
        artifacts: &Path,
        cancel: &CancellationToken,
    ) -> Result<Fetched> {
        let mut current = web::url::normalize(&request.url)?;
        if let Some(mut page) = self.cache.lock().unwrap().get(&request.url, Instant::now()) {
            page.cache_hit = true;
            return Ok(Fetched::Page(page));
        }
        let mut redirects: Vec<Hop> = vec![];
        let mut response = None;
        for _ in 0..=web::MAX_REDIRECTS {
            web::egress::literal_guard(&current)?;
            let next = self
                .get(&current, request.trace_id.as_deref(), cancel)
                .await?;
            if !matches!(next.status, 301 | 302 | 303 | 307 | 308) {
                response = Some(next);
                break;
            }
            let location = header(&next.headers, "location").filter(|l| !l.trim().is_empty());
            let Some(location) = location else {
                return Ok(Fetched::HttpError {
                    final_url: current.to_string(),
                    redirects,
                    retry_after: web::text::retry_after(header(&next.headers, "retry-after"))
                        .map(str::to_owned),
                    status: next.status,
                });
            };
            let target = web::url::resolve_redirect(location, &current)?;
            let shown = web::url::redact_credentials(&target);
            let hop = Hop {
                from: current.to_string(),
                to: shown.clone(),
                status: next.status,
            };
            if !web::url::permitted_redirect(&current, &target) {
                redirects.push(hop);
                return Ok(Fetched::Redirect {
                    original_url: current.to_string(),
                    redirect_url: shown,
                    redirects,
                    status: next.status,
                });
            }
            redirects.push(hop);
            current = target;
        }
        let Some(response) = response else {
            return Err(WebError::new(
                "webfetch_too_many_redirects",
                "WebFetch exceeded the safe redirect limit",
            )
            .into());
        };
        if header(&response.headers, "x-proxy-error") == Some("blocked-by-allowlist") {
            let domain = current.host_str().unwrap_or("");
            let notice = format!("Access to {domain} is blocked by the network egress proxy.");
            // Node JSON.stringify 的键序：error_type、domain、message。
            let message = format!(
                "{{\"error_type\":\"EGRESS_BLOCKED\",\"domain\":{},\"message\":{}}}",
                serde_json::Value::from(domain),
                serde_json::Value::from(notice)
            );
            return Err(WebError::new("webfetch_egress_blocked", message).into());
        }
        if !(200..300).contains(&response.status) {
            return Ok(Fetched::HttpError {
                final_url: current.to_string(),
                redirects,
                retry_after: web::text::retry_after(header(&response.headers, "retry-after"))
                    .map(str::to_owned),
                status: response.status,
            });
        }
        let content_type = header(&response.headers, "content-type")
            .unwrap_or("")
            .to_owned();
        let content = web::html::extract_readable(&response.body, &content_type)?;
        let artifact_path = self.artifact(&content, &content_type, artifacts).await?;
        let page = Page {
            content,
            content_type,
            final_url: current.to_string(),
            status: response.status,
            bytes: response.body.len() as u64,
            redirects,
            artifact_path,
            cache_hit: false,
        };
        self.cache
            .lock()
            .unwrap()
            .put(&request.url, page.clone(), Instant::now());
        Ok(Fetched::Page(page))
    }

    /// Node `maybePersistRawContent`: pages over 100,000 bytes are kept in the
    /// session's tool results.
    async fn artifact(
        &self,
        content: &str,
        content_type: &str,
        artifacts: &Path,
    ) -> Result<Option<String>> {
        if content.len() <= web::MAX_MODEL_INPUT_CHARS {
            return Ok(None);
        }
        let extension = if content_type.contains("html") {
            "md"
        } else {
            "txt"
        };
        let directory = artifacts.join("web-fetch");
        tokio::fs::create_dir_all(&directory).await?;
        let path = directory.join(format!("{}.{extension}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, content).await?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }
}
