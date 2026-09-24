//! What the model reads from WebFetch (Node `webfetch.ts`, `webfetch-processing.ts`).
use serde_json::Value;
use std::sync::OnceLock;

pub const EMPTY_RESULT: &str = "WebFetch completed, but the extraction model returned no text.";
pub const CANCELLED: &str = "WebFetch was cancelled before the page could be processed";
pub const PROCESSING_FAILED: &str = "WebFetch prompt processing failed";

/// Node `httpStatusText`: the response's reason phrase, else `http.STATUS_CODES`.
pub fn status_text(status: u16, reason: &str) -> String {
    static CODES: OnceLock<Value> = OnceLock::new();
    let codes = CODES.get_or_init(|| {
        serde_json::from_str(include_str!("../../schema/http-status.json")).expect("status table")
    });
    let reason = crate::js_string::trim(reason);
    if !reason.is_empty() {
        return reason.to_owned();
    }
    codes[status.to_string()]
        .as_str()
        .unwrap_or("Unknown Status")
        .to_owned()
}

/// Node `formatRedirectOutput` result (the prompt is interpolated as is).
pub fn redirect(
    original: &str,
    target: &str,
    status: u16,
    status_text: &str,
    prompt: &str,
) -> String {
    [
        "REDIRECT DETECTED: The URL redirects to a different host.".to_owned(),
        String::new(),
        format!("Original URL: {original}"),
        format!("Redirect URL: {target}"),
        format!("Status: {status} {status_text}"),
        String::new(),
        "To complete your request, I need to fetch content from the redirected URL. Please use WebFetch again with these parameters:".to_owned(),
        format!("- url: \"{target}\""),
        format!("- prompt: \"{prompt}\""),
    ]
    .join("\n")
}

/// Node `formatHttpErrorOutput` result.
pub fn http_error(status: u16, status_text: &str, retry_after: Option<&str>) -> String {
    let retry = retry_after.map_or(String::new(), |r| format!("\nRetry-After: {r}"));
    format!(
        "The server returned HTTP {status} {status_text}.{retry}\n\nThe response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch."
    )
}

/// Node keeps `Retry-After` only as 1–6 digits.
pub fn retry_after(value: Option<&str>) -> Option<&str> {
    value.filter(|v| (1..=6).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit()))
}

/// Node `buildProcessingPrompt`.
pub fn processing_prompt(content: &str, prompt: &str, preapproved: bool) -> String {
    let instruction = if preapproved {
        "Provide a concise response based on the content above. Include relevant details, code examples, and documentation excerpts as needed."
    } else {
        "Provide a concise response based only on the content above. In your response:
 - Enforce a strict 125-character maximum for quotes from any source document. Open Source Software is ok as long as we respect the license.
 - Use quotation marks for exact language from articles; any language outside of the quotation should never be word-for-word the same.
 - You are not a lawyer and never comment on the legality of your own prompts and responses.
 - Never produce or reproduce exact song lyrics."
    };
    format!("\nWeb page content:\n---\n{content}\n---\n\n{prompt}\n\n{instruction}\n")
}

/// Node `shouldReturnMarkdownDirectly`.
pub fn direct_markdown(preapproved: bool, content_type: &str, content: &str) -> bool {
    preapproved
        && content_type.to_lowercase().contains("text/markdown")
        && content.encode_utf16().count() < super::MAX_MODEL_INPUT_CHARS
}
