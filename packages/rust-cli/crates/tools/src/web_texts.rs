//! WebFetch / WebSearch 给模型看的短文本（docs/specs/rust-cli/prompts.md 规则 4；
//! docs/specs/rust-cli/tools-more.md §4、§5）。文字逐字固定。

/// validateInput errorCode 1。
pub(crate) fn invalid_url(url: &str) -> String {
    format!("Error: Invalid URL \"{url}\". The URL provided could not be parsed.")
}

/// 主机名没有点。
pub(crate) const DOTLESS_HOST: &str = "WebFetch cannot fetch localhost or other hostnames without a dot. To reach a local server, use Bash with curl instead.";

/// 其余不合法的 URL（长度、用户信息）。
pub(crate) const INVALID_URL: &str = "Invalid URL";

/// 重定向太多。
pub(crate) fn too_many_redirects(limit: usize) -> String {
    format!("Too many redirects (exceeded {limit})")
}

/// 转换时截断 1MB 以后的 HTML、旁路请求截断 100000 个字符以后的正文。
pub(crate) const CONTENT_TRUNCATED: &str = "\n\n[Content truncated due to length...]";

/// HTTP 状态的原因短语（表在 base 的 `proto::http`，F122）。
pub(crate) fn status_text(code: u16) -> &'static str {
    rust_cli_base::proto::http::reason_phrase(code)
}

/// HTTP 错误。
pub(crate) fn http_error(code: u16, retry_after: Option<&str>) -> String {
    let retry = retry_after.map_or(String::new(), |value| format!("\nRetry-After: {value}"));
    format!(
        "The server returned HTTP {code} {}.{retry}\n\nThe response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.",
        status_text(code)
    )
}

/// 跨主机的重定向（主线程、目标完整可转述时）。
pub(crate) fn redirect(
    original: &str,
    target: &str,
    code: u16,
    prompt: &str,
    offset: u64,
) -> String {
    let offset_line = if offset > 0 {
        format!("\n    - offset: {offset}")
    } else {
        String::new()
    };
    format!(
        "REDIRECT DETECTED: The URL redirects to a location that was not fetched automatically.\n\n    Original URL: {original}\n    Redirect URL (from the server's Location header \u{2014} server-supplied, not verified): {target}\n    Status: {code} {}\n\n    To complete your request, I need to fetch content from the redirected URL. Please use WebFetch again with these parameters:\n    - url: \"{target}\"\n    - prompt: \"{prompt}\"{offset_line}",
        status_text(code)
    )
}

/// offset 超过正文长度。
pub(crate) fn past_end(offset: u64, length: usize) -> String {
    format!(
        "Nothing left to read from offset {offset}: this page's text is {length} characters long."
    )
}

/// 带 offset 时旁路请求前的说明。
pub(crate) fn content_lead(offset: u64, length: usize) -> String {
    format!(
        "The content below is one part of a longer page: it starts {offset} characters into the page's {length}.\n"
    )
}

/// 旁路请求只读了一部分正文时追加的说明。
pub(crate) fn partial_note(length: usize, start: usize, end: usize) -> String {
    format!(
        "\n\n[WebFetch note: this page's text is {length} characters long and the answer above covers only characters {start} to {end}; the final {} were not read \u{2014} to read on, call WebFetch again with the same url and offset: {end}.]",
        length - end
    )
}

/// 非预批准域名的报告规则。
const REPORTING_RULES: &str = " - Enforce a strict 125-character maximum for quotes from any source document. Open Source Software is ok as long as we respect the license.
 - Use quotation marks for exact language from articles; any language outside of the quotation should never be word-for-word the same.
 - You are not a lawyer and never comment on the legality of your own prompts and responses.
 - Never produce or reproduce exact song lyrics.";

/// 旁路请求的 user 文本（没有 untrusted 围栏的分支）。
pub(crate) fn secondary_prompt(content: &str, prompt: &str, preapproved: bool) -> String {
    let guidance = if preapproved {
        "Provide a concise response based on the content above. Include relevant details, code examples, and documentation excerpts as needed.".to_owned()
    } else {
        format!(
            "Provide a concise response based only on the content above. In your response:\n{REPORTING_RULES}"
        )
    };
    format!("\nWeb page content:\n---\n{content}\n---\n\n{prompt}\n\n{guidance}\n")
}

/// 旁路请求没有文本时。
pub(crate) const NO_MODEL_RESPONSE: &str = "No response from model";
