//! Official MCP identity for stdio servers and the connection status of
//! official failures (Node `mcp/index.ts` `resolveOfficialStdioAuthMeta`,
//! `stdio-transport.ts` `mergeRequestMeta`, `failConnection`). Spec
//! rust-m10-plugins §3.11.
use super::mcp_config::Server;
use super::mcp_official_http::Diagnostic;
use super::official_auth::{META_KEY, Official, OfficialAuth, UNAVAILABLE, UNTRUSTED};
use futures_util::SinkExt;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::async_rw::JsonRpcMessageCodec;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::codec::FramedWrite;
use tokio_util::sync::PollSender;

const WRITE_QUEUE: usize = 32;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// The `_meta` payload of one outbound stdio message: headers or the reason.
async fn payload(auth: &OfficialAuth, official: &Official) -> Value {
    let fail = |reason: &str| {
        tracing::warn!(event = "mcp.official_auth.stdio_meta", mcp_key = %official.mcp_key,
            reason, status = "failed", "Official MCP stdio auth headers unavailable");
        json!({ "ok": false, "reason": reason })
    };
    if !auth.attached() {
        return fail(UNAVAILABLE);
    }
    // stdio 没有 url：目标 origin 由宿主给出，仍过可信判定（https、无凭证、dev 回环开关）。
    let Some(target) = auth.zcode_origin() else {
        return fail(UNAVAILABLE);
    };
    if !auth.is_trusted(&target).0 {
        return fail(UNTRUSTED);
    }
    match auth.headers(official, &target).await {
        Ok(headers) => json!({ "ok": true, "headers": headers }),
        Err(reason) => fail(reason),
    }
}

/// Node `mergeRequestMeta`: requests and notifications get the payload in
/// `params._meta`, overriding any stale value; responses are untouched.
pub(super) fn merge_meta(message: &mut Value, payload: Value) {
    if message.get("method").is_none() {
        return;
    }
    if !message["params"].is_object() {
        message["params"] = json!({});
    }
    if !message["params"]["_meta"].is_object() {
        message["params"]["_meta"] = json!({});
    }
    message["params"]["_meta"][META_KEY] = payload;
}

/// The writer of an official stdio server: messages are queued and written
/// in order by one task that resolves each message's identity payload.
/// Dropping the returned sender ends the task and closes stdin.
pub(super) fn stdio_writer(
    input: tokio::process::ChildStdin,
    auth: Arc<OfficialAuth>,
    official: Official,
) -> PollSender<ClientJsonRpcMessage> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ClientJsonRpcMessage>(WRITE_QUEUE);
    let codec = JsonRpcMessageCodec::<Value>::new_with_max_length(MAX_LINE_BYTES);
    let mut writer = FramedWrite::new(input, codec);
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let Ok(mut value) = serde_json::to_value(message) else {
                break;
            };
            if value.get("method").is_some() {
                merge_meta(&mut value, payload(&auth, &official).await);
            }
            if writer.send(value).await.is_err() {
                break;
            }
        }
    });
    PollSender::new(tx)
}

/// An official connection failure with its classified kind (Node `failConnection`).
#[derive(Debug)]
pub(super) struct ConnectFailure {
    pub kind: &'static str,
    pub request_id: Option<String>,
}

impl std::fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind)
    }
}

impl std::error::Error for ConnectFailure {}

/// Classifies an official connection failure; other failures pass through.
pub(super) fn connect_failure(diagnostic: &Diagnostic, error: anyhow::Error) -> anyhow::Error {
    let kind = if diagnostic.auth_kind == Some(UNTRUSTED) {
        Some("official_origin_untrusted")
    } else {
        diagnostic.failure_kind
    };
    if let Some(kind) = diagnostic.auth_kind {
        tracing::warn!(
            event = "mcp.server.failed",
            official_auth_kind = kind,
            "MCP server connection failed"
        );
    }
    match kind {
        Some(kind) => ConnectFailure {
            kind,
            request_id: diagnostic.request_id.clone(),
        }
        .into(),
        None => error,
    }
}

/// The failed status of a classified official connection.
pub(super) fn failed_status(server: &Server, error: &anyhow::Error) -> Option<Value> {
    let failure = error.downcast_ref::<ConnectFailure>()?;
    let mut status = super::mcp_config::status(server, "failed", 0, Some(failure.kind));
    if let Some(id) = &failure.request_id {
        status["serverRequestId"] = id.clone().into();
        status["error"] = format!("{} - {id}", status["error"].as_str().unwrap_or_default()).into();
    }
    Some(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_is_merged_only_into_requests_and_notifications() {
        let mut request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x","_meta":{"trace_id":"t",META_KEY:"stale"}}});
        merge_meta(
            &mut request,
            json!({"ok":true,"headers":{"Authorization":"a"}}),
        );
        assert_eq!(request["params"]["_meta"]["trace_id"], "t");
        assert_eq!(request["params"]["_meta"][META_KEY]["ok"], true);
        let mut notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        merge_meta(&mut notification, json!({"ok":false,"reason":UNAVAILABLE}));
        assert_eq!(
            notification["params"]["_meta"][META_KEY]["reason"],
            UNAVAILABLE
        );
        let mut response = json!({"jsonrpc":"2.0","id":1,"result":{}});
        merge_meta(&mut response, json!({"ok":true}));
        assert!(response.get("params").is_none());
    }
}
