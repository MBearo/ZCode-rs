//! Streamable HTTP client of official MCP servers (Node `official-auth.ts`
//! `createOfficialMcpAuthFetch`): every request re-checks the target origin,
//! asks the Host for identity headers, never follows redirects and retries a
//! 401 once. Spec rust-m10-plugins §3.11.
use super::official_auth::{Official, OfficialAuth, UNTRUSTED};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::StreamableHttpError;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(super) const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const REQUEST_ID_HEADER: &str = "x-request-id";
const TRACE_ID_HEADER: &str = "x-trace-id";
pub(super) const EVENT_STREAM: &str = "text/event-stream";
pub(super) const JSON_TYPE: &str = "application/json";
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

/// A classified official auth failure (Node `OfficialMcpAuthError`).
#[derive(Debug)]
pub(super) enum Failure {
    /// The classified kind is recorded in [`Diagnostic::auth_kind`].
    Auth(String),
    Http(reqwest::Error),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(message) => f.write_str(message),
            Self::Http(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Failure {}

pub(super) type HttpError = StreamableHttpError<Failure>;

/// Connection-time facts recorded for the server status (Node
/// `lastOfficialAuthKind` and `connectionDiagnosticByServer`).
#[derive(Default, Clone, Debug)]
pub(super) struct Diagnostic {
    pub auth_kind: Option<&'static str>,
    pub failure_kind: Option<&'static str>,
    pub request_id: Option<String>,
}

#[derive(Clone)]
pub(super) struct OfficialHttp {
    pub(super) client: reqwest::Client,
    endpoint_origin: Option<String>,
    official: Official,
    server: String,
    auth: Arc<OfficialAuth>,
    pub diagnostic: Arc<Mutex<Diagnostic>>,
}

fn safe_origin(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (url.username().is_empty() && url.password().is_none())
        .then(|| url.origin().ascii_serialization())
}

/// `(method, tool name)` of an outgoing JSON-RPC message, for classification only.
pub(super) fn describe(message: &ClientJsonRpcMessage) -> (Option<String>, Option<String>) {
    let value = serde_json::to_value(message).unwrap_or(Value::Null);
    let method = value["method"].as_str().map(str::to_owned);
    let tool = value["params"]["name"].as_str().map(str::to_owned);
    (method, tool)
}

impl OfficialHttp {
    pub fn new(
        client: reqwest::Client,
        url: &str,
        official: Official,
        server: &str,
        auth: Arc<OfficialAuth>,
    ) -> Self {
        Self {
            client,
            endpoint_origin: safe_origin(url),
            official,
            server: server.to_owned(),
            auth,
            diagnostic: Arc::default(),
        }
    }

    pub(super) fn fail(&self, kind: &'static str, message: String) -> HttpError {
        self.diagnostic.lock().expect("diagnostic").auth_kind = Some(kind);
        StreamableHttpError::Client(Failure::Auth(message))
    }

    /// Node's per-request origin checks; neither failure sends anything.
    fn target(&self, uri: &str) -> Result<String, HttpError> {
        let origin = safe_origin(uri);
        let Some(origin) = origin.filter(|o| Some(o) == self.endpoint_origin.as_ref()) else {
            let message = format!(
                "official MCP request origin does not match the configured endpoint: {}",
                self.server
            );
            return Err(self.fail(UNTRUSTED, message));
        };
        let (trusted, detail) = self.auth.is_trusted(&origin);
        if !trusted {
            let message = format!(
                "official MCP origin is not trusted ({detail}): {} origin={origin} pluginId={}",
                self.server, self.official.plugin_id
            );
            return Err(self.fail(UNTRUSTED, message));
        }
        Ok(origin)
    }

    /// Merges identity headers over the transport headers (Node `mergeOfficialAuthHeaders`).
    async fn headers(
        &self,
        origin: &str,
        custom: &HashMap<HeaderName, HeaderValue>,
    ) -> (HeaderMap, bool) {
        let identity = match self.auth.headers(&self.official, origin).await {
            Ok(headers) => headers,
            Err(reason) => {
                // 解析不到身份时匿名发送，交给服务端做权威判定；不缓存旧凭证。
                tracing::warn!(event = "mcp.official_auth.resolve", mcp_key = %self.official.mcp_key,
                    reason, fallback = "anonymous", "Official MCP auth headers unavailable for request");
                Default::default()
            }
        };
        let names: Vec<String> = identity.keys().map(|k| k.to_lowercase()).collect();
        let mut merged = HeaderMap::new();
        for (name, value) in custom {
            let lower = name.as_str();
            let reserved = zcode_cli_plugins::mcp_auth::reserved(lower)
                && lower != SESSION_HEADER
                && lower != PROTOCOL_HEADER;
            if names.iter().any(|n| n == lower) || reserved {
                continue;
            }
            merged.insert(name.clone(), value.clone());
        }
        for (name, value) in &identity {
            if let (Ok(name), Some(Ok(value))) = (
                name.parse::<HeaderName>(),
                value.as_str().map(HeaderValue::from_str),
            ) {
                merged.insert(name, value);
            }
        }
        // 关联 id 由服务端生成，插件静态配置不能注入到官方端点。
        merged.remove(REQUEST_ID_HEADER);
        merged.remove(TRACE_ID_HEADER);
        tracing::debug!(event = "mcp.official_auth.request", mcp_key = %self.official.mcp_key,
            identity_header_names = ?names, "Official MCP request sending");
        (merged, !identity.is_empty())
    }

    /// Sends with identity headers; a 401 after injected credentials retries
    /// once. 401, 403 and redirects become classified failures.
    pub(super) async fn exchange<F>(
        &self,
        uri: &str,
        custom: &HashMap<HeaderName, HeaderValue>,
        tool_call: bool,
        build: F,
    ) -> Result<(reqwest::Response, Option<String>), HttpError>
    where
        F: Fn(HeaderMap) -> reqwest::RequestBuilder,
    {
        let origin = self.target(uri)?;
        let mut attempt = 1;
        let response = loop {
            let (headers, applied) = self.headers(&origin, custom).await;
            let response = build(headers)
                .send()
                .await
                .map_err(|e| StreamableHttpError::Client(Failure::Http(e)))?;
            if response.status() == 401 && applied && attempt == 1 {
                tracing::info!(
                    event = "mcp.official_auth.request",
                    attempt = 2,
                    "Official MCP retrying once after 401"
                );
                attempt += 1;
                continue;
            }
            break response;
        };
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        let status = response.status().as_u16();
        let suffix = |message: &str| match (&request_id, tool_call) {
            (Some(id), true) => format!("{message} - {id}"),
            _ => message.to_owned(),
        };
        if !response.status().is_success() {
            tracing::warn!(event = "mcp.official_auth.request", http_status = status,
                server_request_id = ?request_id, "Official MCP response failed");
        }
        let (kind, message) = match status {
            401 => (
                "official_auth_rejected",
                suffix("official MCP rejected the current credential"),
            ),
            403 => (
                "official_auth_forbidden",
                suffix("official MCP denied access for the current plan"),
            ),
            300..=399 => (
                "official_auth_redirect_blocked",
                suffix(&format!(
                    "official MCP responded with a blocked redirect ({status})"
                )),
            ),
            _ => return Ok((response, request_id)),
        };
        // 分类在抛出前完成：连接期诊断仍记录响应体给出的失败类型。
        if !tool_call {
            let content_type = content_type(&response);
            let body = response.bytes().await.unwrap_or_default();
            self.record(status, content_type.as_deref(), &body, request_id);
        }
        Err(self.fail(kind, message))
    }

    /// Node `rememberServerResponse`: a classified connection-time response
    /// keeps its kind and the server's request id.
    pub(super) fn record(
        &self,
        status: u16,
        content_type: Option<&str>,
        body: &[u8],
        request_id: Option<String>,
    ) {
        if let Some(kind) = classify(status, content_type, body) {
            let mut diagnostic = self.diagnostic.lock().expect("diagnostic");
            diagnostic.failure_kind = Some(kind);
            diagnostic.request_id = request_id;
        }
    }
}

pub(super) fn content_type(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Node `classifyOfficialMcpResponse` of a connection-time body.
fn classify(status: u16, content_type: Option<&str>, body: &[u8]) -> Option<&'static str> {
    let fallback = (!(200..300).contains(&status)).then_some("connection_failed");
    let json = content_type.is_some_and(|t| t.to_lowercase().contains("json"));
    match status {
        429 => Some("rate_limited"),
        500.. => Some("server_internal_error"),
        _ if !json || body.is_empty() || body.len() > MAX_DIAGNOSTIC_BYTES => fallback,
        _ => match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(record)) => match (record.get("code"), record.get("error")) {
                (Some(code), _) if code == 3001 => Some("server_not_found"),
                (Some(code), _) if code == 1000 => Some("server_unavailable"),
                (_, Some(error)) if record.get("jsonrpc") == Some(&Value::from("2.0")) => {
                    match error["code"].as_i64() {
                        Some(1006) => Some("not_authenticated"),
                        Some(3101) => Some("coding_plan_required"),
                        _ => Some("protocol_error"),
                    }
                }
                _ => fallback,
            },
            _ => fallback,
        },
    }
}
