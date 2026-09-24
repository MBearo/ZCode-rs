//! WebFetch without IO (Node `core/src/tool/handlers/webfetch-*.ts`): URL
//! rules, the literal-IP egress guard, readable extraction and model texts.
//! Spec rust-m5-tools §3.
pub mod egress;
pub mod html;
pub mod search;
pub mod text;
pub mod url;

pub const MAX_URL_CHARS: usize = 2_000;
pub const MAX_RESPONSE_BYTES: u64 = 10 * 1024 * 1024;
/// UTF-16 units sent to the processing model, and the raw-artifact threshold in bytes.
pub const MAX_MODEL_INPUT_CHARS: usize = 100_000;
pub const CACHE_TTL_MS: u64 = 15 * 60 * 1000;
pub const CACHE_MAX_BYTES: usize = 50 * 1024 * 1024;
/// Redirects followed after the first GET (at most 11 requests).
pub const MAX_REDIRECTS: usize = 10;
pub const TIMEOUT_MS: u64 = 60_000;
pub const USER_AGENT: &str = "ZCode-WebFetch/0.1 (+https://zcode.ai; coding-agent-cli)";
pub const ACCEPT: &str = "text/markdown, text/html, */*";

/// A WebFetch failure; `code` is Node's `webFetchCode` (`None` for plain errors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebError {
    pub code: Option<&'static str>,
    pub message: String,
}

impl WebError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WebError {}

/// WebFetch's schema check (not strict: unknown keys are dropped first).
pub fn fetch_validation(args: &serde_json::Value) -> Result<(), String> {
    use crate::zod::{Schema, string};
    let schema = Schema::Object(vec![("url", string(), false), ("prompt", string(), false)]);
    let known: serde_json::Map<String, serde_json::Value> = args
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| matches!(k.as_str(), "url" | "prompt"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (_, issues) = schema.run(Some(&serde_json::Value::Object(known)));
    if issues.is_empty() {
        return Ok(());
    }
    Err(crate::tool_input::render("WebFetch", &issues))
}

#[cfg(test)]
mod tests;

/// What the network side needs for one WebFetch call.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub session: String,
    /// The model's raw `url` (also the cache key).
    pub url: String,
    pub trace_id: Option<String>,
}

/// One followed redirect (Node `WebFetchRedirect`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Hop {
    pub from: String,
    pub to: String,
    pub status: u16,
}

/// A readable page (Node `CachedFetchContent`).
#[derive(Clone, Debug)]
pub struct Page {
    pub content: String,
    pub content_type: String,
    pub final_url: String,
    pub status: u16,
    /// Response body bytes.
    pub bytes: u64,
    pub redirects: Vec<Hop>,
    /// The extracted content saved for pages over 100,000 bytes.
    pub artifact_path: Option<String>,
    pub cache_hit: bool,
}

/// Node `FetchAndExtractContentResult`.
#[derive(Clone, Debug)]
pub enum Fetched {
    Page(Page),
    /// A redirect to another host, returned to the model instead of followed.
    Redirect {
        original_url: String,
        redirect_url: String,
        redirects: Vec<Hop>,
        status: u16,
    },
    /// A non-2xx response; the body is not used.
    HttpError {
        final_url: String,
        redirects: Vec<Hop>,
        retry_after: Option<String>,
        status: u16,
    },
}
