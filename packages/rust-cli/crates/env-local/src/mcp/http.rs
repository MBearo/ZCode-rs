//! streamable HTTP 与 SSE 传输（docs/specs/rust-cli/ext.md §3.2；MCP SDK `StreamableHTTPClientTransport`、
//! `SSEClientTransport`），请求经 `rust-cli-net` 的 [`Egress`]。
//!
//! - streamable HTTP：每条消息一个 POST；响应是 JSON 或 SSE 流（流在后台任务里读）；记下
//!   `mcp-session-id`；`notifications/initialized` 发出后开一个 GET SSE 流收服务器消息（405 时不开）；
//!   带会话 ID 的请求得到 404 时会话已过期，送出 `Closed{session_expired}`。
//! - SSE：GET 得到事件流，第一个 `endpoint` 事件给出 POST 地址；响应经事件流的 `message` 事件到达。
//!
//! 会话结束时应发 `DELETE`；net 还没有 DELETE，这里不发（ext.md §8）。
//!
//! 事件通道有界（审查 R1-18）：读响应流的任务在通道满时停读（背压到服务器）；`send` 自己收到的 JSON 响应与
//! 会话过期交给一个后台任务送出——`send` 可能正由事件的唯一读者调用（回答服务器的请求），在这里等通道就是
//! 自己等自己。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rust_cli_base::OwnedTask;
use rust_cli_base::proto::http;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::units;
use rust_cli_kernel_api::mcp::{McpError, McpTransportEvent};
use rust_cli_net::header::{ACCEPT, CONTENT_TYPE, EVENT_STREAM, JSON};
use rust_cli_net::sse::SseDecoder;
use rust_cli_net::{ERROR_BODY_LIMIT, Egress, HeaderValue, HttpRequest, HttpResponse};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Streamable HTTP 的会话头（MCP 规范；请求带上、响应里读，F55：以前写了两次）。
const SESSION_HEADER: &str = "mcp-session-id";
/// JSON 响应体的上限。
const BODY_LIMIT: usize = 64 * units::MIB;

type Events = super::EventSender;

/// 一条 HTTP 连接（streamable 或 SSE）。
pub(crate) struct HttpLink {
    egress: Arc<Egress>,
    headers: BTreeMap<String, String>,
    /// streamable：服务器 URL；SSE：`endpoint` 事件给出的 POST 地址。
    post_url: String,
    streamable: bool,
    session: Mutex<Option<String>>,
    events: Events,
    /// 后台读响应流的任务；连接释放时一起中止。
    readers: Mutex<Vec<OwnedTask<()>>>,
    closed: CancellationToken,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn failed(message: String) -> McpError {
    McpError::Connect { message }
}

/// 加上配置给的 header。
///
/// 缺陷修复 R2-13：值原来按普通值交给 net；配置里的 header 常是 Authorization 或 API key，现在一律是秘密值
/// （发送时标为敏感，任何 Debug 输出都看不到原值）。
fn with_headers(request: HttpRequest, headers: &BTreeMap<String, String>) -> HttpRequest {
    headers.iter().fold(request, |request, (name, value)| {
        request.header(
            name.as_str(),
            HeaderValue::Secret(RedactedString::new(value.as_str())),
        )
    })
}

fn messages(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        other => vec![other],
    }
}

/// 读一个 SSE 流，把 `message`（或没有名字的）事件交出去；`endpoint` 事件交给 `endpoint`。
async fn read_sse(
    mut response: HttpResponse,
    events: Events,
    mut endpoint: Option<oneshot::Sender<String>>,
) -> Result<(), String> {
    let mut decoder = SseDecoder::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let found = decoder.push(&chunk).map_err(|error| error.to_string())?;
        for event in found {
            match event.event.as_deref() {
                Some("endpoint") => {
                    if let Some(sender) = endpoint.take() {
                        let _ = sender.send(event.data);
                    }
                }
                None | Some("message") => match serde_json::from_str::<Value>(&event.data) {
                    Ok(value) => {
                        for message in messages(value) {
                            // 通道满时等（背压）；读者已经释放事件流时不再读这个流。
                            if events
                                .send(McpTransportEvent::Message { message })
                                .await
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => tracing::debug!(%error, "ignored a non-JSON MCP SSE event"),
                },
                Some(other) => tracing::debug!(event = other, "ignored an MCP SSE event"),
            }
        }
    }
}

impl HttpLink {
    fn request(&self, request: HttpRequest) -> HttpRequest {
        let mut request = with_headers(request, &self.headers);
        if let Some(session) = lock(&self.session).clone() {
            request = request.header(SESSION_HEADER, session);
        }
        request
    }

    fn keep(&self, task: OwnedTask<()>) {
        let mut readers = lock(&self.readers);
        readers.retain(|reader| !reader.is_finished());
        readers.push(task);
    }

    /// 在后台按顺序送出这些事件（通道满时等，不卡住 `send` 的调用方）。
    fn deliver(&self, batch: Vec<McpTransportEvent>) {
        let events = self.events.clone();
        self.keep(rust_cli_base::owned(async move {
            for event in batch {
                if events.send(event).await.is_err() {
                    return;
                }
            }
        }));
    }

    /// 收服务器主动发来的消息（streamable 的 GET 流）。405 表示服务器不提供，静默跳过。
    async fn open_listener(&self) {
        let request = self.request(HttpRequest::get(&self.post_url).header(ACCEPT, EVENT_STREAM));
        match self.egress.send(request).await {
            Ok(response) if response.is_success() => {
                let events = self.events.clone();
                self.keep(rust_cli_base::owned(async move {
                    if let Err(error) = read_sse(response, events, None).await {
                        tracing::debug!(error, "MCP listener stream ended");
                    }
                }));
            }
            Ok(response) => {
                tracing::debug!(
                    status = response.status(),
                    "MCP server has no listener stream"
                );
            }
            Err(error) => tracing::debug!(%error, "MCP listener stream failed"),
        }
    }

    pub(crate) async fn send(&self, message: Value) -> Result<(), McpError> {
        if self.closed.is_cancelled() {
            return Err(McpError::Closed {
                message: "connection closed".to_owned(),
            });
        }
        let initialized =
            message.get("method").and_then(Value::as_str) == Some("notifications/initialized");
        let request = self.request(
            HttpRequest::post(&self.post_url, message.to_string())
                .header(CONTENT_TYPE, JSON)
                .header(ACCEPT, "application/json, text/event-stream"),
        );
        let response = self
            .egress
            .send(request)
            .await
            .map_err(|error| failed(error.to_string()))?;
        let status = response.status();
        if let Some(session) = response.headers().get(SESSION_HEADER) {
            *lock(&self.session) = Some(session.to_owned());
        }
        if status == http::NOT_FOUND && self.streamable && lock(&self.session).is_some() {
            self.deliver(vec![McpTransportEvent::Closed {
                reason: Some("MCP session expired".to_owned()),
                session_expired: true,
            }]);
            return Err(McpError::Closed {
                message: "MCP session expired".to_owned(),
            });
        }
        if !response.is_success() {
            let body = response.text(ERROR_BODY_LIMIT).await.unwrap_or_default();
            return Err(failed(format!("HTTP {status}: {body}")));
        }
        if initialized && self.streamable {
            self.open_listener().await;
        }
        if !self.streamable || status == http::ACCEPTED {
            return Ok(());
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with(EVENT_STREAM) {
            let events = self.events.clone();
            self.keep(rust_cli_base::owned(async move {
                if let Err(error) = read_sse(response, events, None).await {
                    tracing::debug!(error, "MCP response stream ended early");
                }
            }));
        } else if content_type.starts_with(JSON) {
            let body = response
                .text(BODY_LIMIT)
                .await
                .map_err(|error| failed(error.to_string()))?;
            let value: Value = serde_json::from_str(&body)
                .map_err(|error| failed(format!("invalid JSON from the MCP server: {error}")))?;
            self.deliver(
                messages(value)
                    .into_iter()
                    .map(|message| McpTransportEvent::Message { message })
                    .collect(),
            );
        }
        Ok(())
    }

    pub(crate) async fn close(&self) {
        self.closed.cancel();
        lock(&self.readers).clear();
    }
}

/// 连接任务：等关闭，送出 `Closed`。
async fn until_closed(closed: CancellationToken, events: Events) {
    closed.cancelled().await;
    let _ = events
        .send(McpTransportEvent::Closed {
            reason: None,
            session_expired: false,
        })
        .await;
}

/// streamable HTTP：建连不发请求（第一条消息是 `initialize`）。
pub(crate) fn streamable(
    egress: Arc<Egress>,
    url: &str,
    headers: &BTreeMap<String, String>,
    events: Events,
) -> (Arc<HttpLink>, OwnedTask<()>) {
    let closed = CancellationToken::new();
    let link = Arc::new(HttpLink {
        egress,
        headers: headers.clone(),
        post_url: url.to_owned(),
        streamable: true,
        session: Mutex::default(),
        events: events.clone(),
        readers: Mutex::default(),
        closed: closed.clone(),
    });
    (link, rust_cli_base::owned(until_closed(closed, events)))
}

/// SSE：GET 事件流，等 `endpoint` 事件；连接任务读这个流直到结束。
pub(crate) async fn sse(
    egress: Arc<Egress>,
    url: &str,
    headers: &BTreeMap<String, String>,
    events: Events,
) -> Result<(Arc<HttpLink>, OwnedTask<()>), McpError> {
    let request = with_headers(HttpRequest::get(url), headers).header(ACCEPT, EVENT_STREAM);
    let response = egress
        .send(request)
        .await
        .map_err(|error| failed(error.to_string()))?;
    if !response.is_success() {
        let status = response.status();
        let body = response.text(ERROR_BODY_LIMIT).await.unwrap_or_default();
        return Err(failed(format!("HTTP {status}: {body}")));
    }
    let (endpoint, found) = oneshot::channel();
    let closed = CancellationToken::new();
    let stream_events = events.clone();
    let stop = closed.clone();
    let task = rust_cli_base::owned(async move {
        let reason = tokio::select! {
            read = read_sse(response, stream_events.clone(), Some(endpoint)) => {
                Some(read.err().unwrap_or_else(|| "MCP SSE stream ended".to_owned()))
            }
            () = stop.cancelled() => None,
        };
        let _ = stream_events
            .send(McpTransportEvent::Closed {
                reason,
                session_expired: false,
            })
            .await;
    });
    let endpoint = found
        .await
        .map_err(|_| failed("the SSE stream ended before the endpoint event".to_owned()))?;
    let base = url::Url::parse(url)
        .map_err(|error| failed(format!("invalid MCP endpoint {endpoint}: {error}")))?;
    let post_url = base
        .join(&endpoint)
        .map_err(|error| failed(format!("invalid MCP endpoint {endpoint}: {error}")))?;
    // 缺陷修复 R2-5：`endpoint` 原来可以指向别的 origin，配置的 header（常有 Authorization）会跟着 POST 过去。
    // 按 MCP SDK `SSEClientTransport`：origin 不同时连接失败（文字相同），事件流随连接任务一起结束。
    if post_url.origin() != base.origin() {
        return Err(failed(format!(
            "Endpoint origin does not match connection origin: {}",
            post_url.origin().ascii_serialization()
        )));
    }
    let link = Arc::new(HttpLink {
        egress,
        headers: headers.clone(),
        post_url: post_url.to_string(),
        streamable: false,
        session: Mutex::default(),
        events,
        readers: Mutex::default(),
        closed,
    });
    Ok((link, task))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
