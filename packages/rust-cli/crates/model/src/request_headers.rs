//! 模型请求的头（工作线 F64，docs/specs/rust-cli/model.md §2.4）：来源头的组装（照 Node `buildCliZCodeSourceHeaders`，
//! `apps/zcode-cli/packages/bootstrap/src/model-config.ts` 与 `runtime-platform-headers.ts`）与各层的合并顺序。
//!
//! ```text
//! ① 适配器自己的头 → ② 凭据 → ③ 来源头（+ OpenRouter） → ④ Provider 配置 → ⑤ 每请求头
//! ```
//!
//! 同名时后来的替换旧的，名字不分大小写（Node `mergeModelRequestHeaders`）。修 bug（F64）：以前 `user-agent` 在 Provider
//! 配置之后加，net 发送时后写的覆盖前写的，Provider 配置的 `User-Agent` 被换掉。

use std::sync::Arc;

use rust_cli_base::rule_url::is_openrouter_endpoint;
use rust_cli_kernel_api::provider::{ModelEndpoint, WireProtocol};
use rust_cli_net::{HeaderValue, HttpRequest, header};

use crate::anthropic::BETA_HEADER;
use crate::attribution::{self, Origin};
use crate::protocol::Protocol;

/// 每次发送取一个新的 `x-request-id`（入口给 `NodeIds` 的 uuid）。
pub type RequestIdSource = Arc<dyn Fn() -> String + Send + Sync>;

/// 入口给的来源头与 ID（`ProviderModel::with_request_headers`）。没有时（测试）这两层都不发。
#[derive(Clone)]
pub struct RequestHeaders {
    /// 来源头：[`source_headers`] 的结果，进程启动时算一次。
    pub source: Vec<(String, String)>,
    /// `x-zcode-trace-id`：每个进程一个。
    pub trace_id: String,
    /// `x-request-id` 的来源：每次发送（含重试）调用一次。
    pub request_id: RequestIdSource,
}

/// 来源头的输入：入口从环境变量与系统读出（cli `request_headers.rs`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFacts {
    /// 版本：设了 `ZCODE_APP_VERSION` 时是它的原值（空值也算设了），否则本工程的版本。
    pub version: String,
    /// ZCode 服务的 origin（`http-referer`）。
    pub endpoint_origin: String,
    /// `test` 或 `production`。
    pub release_channel: String,
    /// ICU 缺省 locale 的 BCP 47 写法；没有时 `unknown`。
    pub locale: Option<String>,
    /// IANA 时区名；没有时 `unknown`。
    pub time_zone: Option<String>,
    /// Node 的 `process.platform`。
    pub platform: String,
    /// Node 的 `os.arch()`。
    pub arch: String,
    /// Node 的 `os.release()`；没有时不发 `x-os-version`。
    pub os_release: Option<String>,
}

const UNKNOWN: &str = "unknown";

/// 可打印的 ASCII（空格到 `~`）。
const PRINTABLE_ASCII: std::ops::RangeInclusive<u8> = b' '..=b'~';

/// Node `normalizePrintableHeaderValue`：去掉首尾空白后非空、只含 0x20–0x7e 的字符时是它，否则 None。
pub(crate) fn printable(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed.bytes().all(|byte| PRINTABLE_ASCII.contains(&byte)))
        .then_some(trimmed)
}

/// Node `normalizeOsCategory`。
fn os_category(platform: &str) -> &'static str {
    match platform {
        "darwin" => "macos",
        "win32" => "windows",
        _ => "linux",
    }
}

/// 来源头（键序同 Node 的对象；名字是小写，线上本来就是小写）。
pub fn source_headers(facts: &SourceFacts) -> Vec<(String, String)> {
    let version = printable(&facts.version);
    let printable_or_unknown = |value: &Option<String>| {
        value
            .as_deref()
            .and_then(printable)
            .unwrap_or(UNKNOWN)
            .to_owned()
    };
    let mut out = vec![
        ("http-referer", facts.endpoint_origin.clone()),
        (
            header::USER_AGENT,
            header::zcode_user_agent(version.unwrap_or(UNKNOWN)),
        ),
    ];
    if let Some(version) = version {
        out.push(("x-zcode-app-version", version.to_owned()));
    }
    out.extend([
        ("x-title", "Z Code@cli".to_owned()),
        ("x-release-channel", facts.release_channel.clone()),
        ("x-client-language", printable_or_unknown(&facts.locale)),
        ("x-client-timezone", printable_or_unknown(&facts.time_zone)),
        ("x-zcode-agent", "glm".to_owned()),
    ]);
    if let (Some(platform), Some(arch)) = (printable(&facts.platform), printable(&facts.arch)) {
        out.push(("x-platform", format!("{platform}-{arch}")));
    }
    out.push(("x-os-category", os_category(&facts.platform).to_owned()));
    if let Some(release) = facts.os_release.as_deref().and_then(printable) {
        out.push(("x-os-version", release.to_owned()));
    }
    out.into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

/// OpenRouter 端点另带的两个归因头（Node `OPENROUTER_ATTRIBUTION_HEADERS`）。
const OPENROUTER: [(&str, &str); 2] = [
    ("x-openrouter-title", "ZCode"),
    ("x-openrouter-categories", "programming-app"),
];

/// 按名字（不分大小写）换掉已有的，追加在末尾。
fn put(headers: &mut Vec<(String, HeaderValue)>, name: &str, value: HeaderValue) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value));
}

/// beta 列表：按 `,` 拆开、去首尾空白、去掉空项。
fn beta_items(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// Provider 配置的 `anthropic-beta` 与算出的取并集：先算出的、再配置的（转小写），去重。依据：AI SDK 3.0.81 的
/// `getBetasFromHeaders` 把配置里的 beta 并进它算出的。拆开后为空时原样发配置的值（SDK 不写这个头，配置头留在原处）。
fn merge_betas(headers: &mut Vec<(String, HeaderValue)>, name: &str, configured: &str) {
    let mut betas: Vec<String> = headers
        .iter()
        .filter(|(existing, _)| existing.eq_ignore_ascii_case(BETA_HEADER))
        .filter_map(|(_, value)| match value {
            HeaderValue::Plain(value) => Some(value.as_str()),
            HeaderValue::Secret(_) => None,
        })
        .flat_map(beta_items)
        .map(str::to_owned)
        .collect();
    for item in beta_items(configured) {
        let item = item.to_lowercase();
        if !betas.contains(&item) {
            betas.push(item);
        }
    }
    let value = if betas.is_empty() {
        HeaderValue::Secret(rust_cli_base::secret::RedactedString::new(configured))
    } else {
        HeaderValue::Plain(betas.join(","))
    };
    put(
        headers,
        if betas.is_empty() { name } else { BETA_HEADER },
        value,
    );
}

/// 这次发送的完整请求：编码结果（①）加上凭据（②）、来源头（③）、Provider 配置（④）与每请求头（⑤）。请求体是
/// `Bytes`，克隆不复制内容。`headers` 为 None 时没有 ③ 与 ⑤。
pub(crate) fn outbound(
    protocol: &dyn Protocol,
    encoded: &HttpRequest,
    endpoint: &ModelEndpoint,
    headers: Option<&RequestHeaders>,
    origin: &Origin,
) -> HttpRequest {
    let mut http = encoded.clone();
    for (name, value) in protocol.credential_headers(&endpoint.credential) {
        put(&mut http.headers, &name, value);
    }
    if let Some(headers) = headers {
        for (name, value) in &headers.source {
            put(&mut http.headers, name, HeaderValue::Plain(value.clone()));
        }
        if is_openrouter_endpoint(&endpoint.base_url) {
            for (name, value) in OPENROUTER {
                put(
                    &mut http.headers,
                    name,
                    HeaderValue::Plain(value.to_owned()),
                );
            }
        }
    }
    for (name, value) in &endpoint.headers {
        if name.eq_ignore_ascii_case(attribution::QUERY_SOURCE) {
            continue;
        }
        if endpoint.protocol == WireProtocol::AnthropicMessages
            && name.eq_ignore_ascii_case(BETA_HEADER)
        {
            merge_betas(&mut http.headers, name, value.expose());
            continue;
        }
        put(&mut http.headers, name, HeaderValue::Secret(value.clone()));
    }
    if let Some(headers) = headers {
        let request_id = (headers.request_id)();
        for (name, value) in
            attribution::headers(origin, &endpoint.base_url, &headers.trace_id, request_id)
        {
            put(&mut http.headers, name, HeaderValue::Plain(value));
        }
    }
    http
}

#[cfg(test)]
#[path = "request_headers_tests.rs"]
mod tests;
