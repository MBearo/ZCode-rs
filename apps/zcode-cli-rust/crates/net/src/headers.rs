//! Model request headers: identity defaults (`bootstrap/src/model-config.ts`),
//! case-insensitive merging (`model-request-headers.ts`), OpenRouter attribution
//! and per-request attribution (`runner-attribution.ts`).
use crate::platform;
use url::Url;

/// Ordered header list where names compare case-insensitively and a later value
/// replaces every earlier case variant (Node `mergeModelRequestHeaders`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.0.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
        self.0.push((name, value.into()));
    }

    pub fn extend<K: Into<String>, V: Into<String>>(
        &mut self,
        other: impl IntoIterator<Item = (K, V)>,
    ) {
        for (name, value) in other {
            self.set(name, value);
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }
}

/// Node `normalizePrintableHeaderValue`.
pub(crate) fn printable(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.bytes().all(|b| (0x20..=0x7e).contains(&b)))
        .map(str::to_owned)
}

pub const DEFAULT_ENDPOINT_ORIGIN: &str = "https://zcode.z.ai";

/// Node `resolveRuntimeZCodeEndpointOrigin`.
pub fn endpoint_origin<'a>(env: impl Fn(&str) -> Option<&'a str>) -> Result<String, String> {
    let configured = ["ZCODE_BASE_URL", "ZCODE_ENDPOINT_ORIGIN"]
        .into_iter()
        .find_map(|key| env(key).map(str::trim).filter(|v| !v.is_empty()));
    let Some(configured) = configured else {
        return Ok(DEFAULT_ENDPOINT_ORIGIN.into());
    };
    let url = Url::parse(configured).map_err(|_| "Invalid URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("ZCode endpoint origin must use http or https".into());
    }
    Ok(url.origin().ascii_serialization())
}

/// Node `buildCliZCodeSourceHeaders`; `title` is `electron` for the app server.
pub fn identity<'a>(
    env: impl Fn(&str) -> Option<&'a str> + Copy,
    title: &str,
    app_version: &str,
) -> Result<Headers, String> {
    let version = printable(Some(env("ZCODE_APP_VERSION").unwrap_or(app_version)));
    let channel = if env("ZCODE_ENV").is_some_and(|v| v.trim().eq_ignore_ascii_case("test")) {
        "test"
    } else {
        "production"
    };
    let mut headers = Headers::default();
    headers.set("HTTP-Referer", endpoint_origin(env)?);
    headers.set(
        "User-Agent",
        format!("ZCode/{}", version.as_deref().unwrap_or("unknown")),
    );
    if let Some(version) = &version {
        headers.set("X-ZCode-App-Version", version);
    }
    headers.set("X-Title", format!("Z Code@{title}"));
    headers.set("X-Release-Channel", channel);
    headers.set(
        "X-Client-Language",
        printable(Some(&platform::language(env))).unwrap_or_else(|| "unknown".into()),
    );
    headers.set(
        "X-Client-Timezone",
        printable(platform::timezone(env).as_deref()).unwrap_or_else(|| "unknown".into()),
    );
    headers.set("X-ZCode-Agent", "glm");
    let os = platform::os();
    if let (Some(name), Some(arch)) = (printable(Some(os.platform)), printable(Some(os.arch))) {
        headers.set("X-Platform", format!("{name}-{arch}"));
    }
    headers.set("X-Os-Category", os.category);
    if let Some(release) = printable(os.release.as_deref()) {
        headers.set("X-Os-Version", release);
    }
    Ok(headers)
}

/// Node `isOpenRouterBaseUrl`.
pub fn is_openrouter(base_url: &str) -> bool {
    Url::parse(base_url.trim()).is_ok_and(|url| {
        let host = url.host_str().unwrap_or("").to_lowercase();
        url.scheme() == "https" && (host == "openrouter.ai" || host.ends_with(".openrouter.ai"))
    })
}

/// Node `withOpenRouterAttributionHeaders`.
pub fn with_openrouter(headers: &mut Headers, base_url: &str) {
    if is_openrouter(base_url) {
        headers.set("X-OpenRouter-Title", "ZCode");
        headers.set("X-OpenRouter-Categories", "programming-app");
    }
}

/// Node `isOpenCodeGoBaseUrl`.
fn is_opencode_go(base_url: &str) -> bool {
    Url::parse(base_url.trim()).is_ok_and(|url| {
        let host = url.host_str().unwrap_or("").to_lowercase();
        let path = url.path().trim_end_matches('/').to_lowercase();
        (host == "opencode.ai" || host.ends_with(".opencode.ai")) && path == "/zen/go/v1"
    })
}

/// Node `stripHeaderInternalPrefixes`.
fn strip_prefixes(value: &str, prefixes: &[&str]) -> String {
    let mut stripped = value;
    for prefix in prefixes {
        if let Some(rest) = stripped.strip_prefix(prefix).filter(|r| !r.is_empty()) {
            stripped = rest;
        }
    }
    if stripped.is_empty() { value } else { stripped }.to_owned()
}

/// Node `normalizeModelSessionIdForAttribution`.
pub fn session_for_attribution(session: Option<&str>) -> Option<String> {
    session
        .filter(|s| !s.is_empty())
        .map(|s| strip_prefixes(s, &["sess_", "subagent_agent_"]))
}

/// Per-request fields that identify where a model call comes from.
pub struct Attribution<'a> {
    pub request_id: &'a str,
    /// `main`, `subagent` or `other`.
    pub session_type: &'a str,
    pub trace_id: &'a str,
    pub query_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub base_url: &'a str,
}

/// Node `createModelRequestAttributionHeaders`.
pub fn attribution(input: &Attribution<'_>) -> Headers {
    let session = session_for_attribution(input.session_id);
    let query = input
        .query_id
        .filter(|q| !q.is_empty())
        .map(|q| strip_prefixes(q, &["query_"]));
    let session_type = match input.session_type {
        "main" | "subagent" | "other" => input.session_type,
        _ => "other",
    };
    let mut headers = Headers::default();
    headers.set("x-request-id", input.request_id);
    headers.set("x-zcode-session-type", session_type);
    headers.set("x-zcode-trace-id", input.trace_id);
    if let Some(query) = query {
        headers.set("x-query-id", query);
    }
    if let Some(session) = &session {
        headers.set("x-session-id", session);
        if is_opencode_go(input.base_url) {
            headers.set("x-opencode-session", session);
        }
    }
    headers
}
