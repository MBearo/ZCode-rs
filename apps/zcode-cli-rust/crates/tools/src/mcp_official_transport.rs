//! The rmcp `StreamableHttpClient` of official MCP servers: every POST, GET
//! and DELETE goes through [`OfficialHttp::exchange`]; responses convert like
//! rmcp's reqwest client. Spec rust-m10-plugins §3.11.
use super::mcp_official_http::{
    EVENT_STREAM, Failure, HttpError, JSON_TYPE, OfficialHttp, SESSION_HEADER, content_type,
    describe,
};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, JsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_client::{
    SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use std::collections::HashMap;
use std::sync::Arc;

const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// The SSE body bounded to `max` bytes per event (rmcp `bounded_sse_stream`).
fn sse(
    response: reqwest::Response,
    max: usize,
) -> BoxStream<'static, Result<sse_stream::Sse, SseError>> {
    let mut size = 0usize;
    let mut newlines = 0u8;
    let bytes = response.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(std::io::Error::other)?;
        for byte in chunk.iter() {
            match byte {
                b'\r' => {}
                b'\n' => {
                    newlines += 1;
                    if newlines >= 2 {
                        size = 0;
                    }
                }
                _ => {
                    newlines = 0;
                    size += 1;
                }
            }
            if size > max {
                return Err(std::io::Error::other(format!(
                    "SSE event exceeds {max} bytes"
                )));
            }
        }
        Ok(chunk)
    });
    sse_stream::SseStream::from_bytes_stream(bytes).boxed()
}

impl StreamableHttpClient for OfficialHttp {
    type Error = Failure;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        custom: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth,
            custom,
            MAX_SSE_EVENT_BYTES,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth: Option<String>,
        custom: HashMap<HeaderName, HeaderValue>,
        max: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let (method, _) = describe(&message);
        let tool_call = method.as_deref() == Some("tools/call");
        let (response, request_id) = self
            .exchange(&uri, &custom, tool_call, |headers| {
                let mut request = self
                    .client
                    .post(uri.as_ref())
                    .header(
                        reqwest::header::ACCEPT,
                        format!("{EVENT_STREAM}, {JSON_TYPE}"),
                    )
                    .headers(headers)
                    .json(&message);
                if let Some(session) = &session_id {
                    request = request.header(SESSION_HEADER, session.as_ref());
                }
                request
            })
            .await?;
        let status = response.status();
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == 404 && session_id.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        let content_type = content_type(&response);
        let session = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let awaits_reply = matches!(message, JsonRpcMessage::Request(_));
        if content_type
            .as_deref()
            .is_some_and(|t| t.starts_with(EVENT_STREAM))
            && status.is_success()
        {
            return Ok(StreamableHttpPostResponse::Sse(sse(response, max), session));
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| StreamableHttpError::Client(Failure::Http(e)))?;
        if !tool_call {
            self.record(status.as_u16(), content_type.as_deref(), &body, request_id);
        }
        if status.is_success() && body.is_empty() && !awaits_reply {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let json = content_type
            .as_deref()
            .is_some_and(|t| t.starts_with(JSON_TYPE));
        match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
            Ok(parsed)
                if json && (status.is_success() || matches!(parsed, JsonRpcMessage::Error(_))) =>
            {
                Ok(StreamableHttpPostResponse::Json(parsed, session))
            }
            _ if status.is_success() && !awaits_reply => Ok(StreamableHttpPostResponse::Accepted),
            _ if !status.is_success() => Err(StreamableHttpError::UnexpectedServerResponse(
                format!("HTTP {status}: {}", String::from_utf8_lossy(&body)).into(),
            )),
            _ => Err(StreamableHttpError::UnexpectedContentType(content_type)),
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth: Option<String>,
        custom: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        let (response, _) = self
            .exchange(&uri, &custom, false, |headers| {
                self.client
                    .delete(uri.as_ref())
                    .headers(headers)
                    .header(SESSION_HEADER, session_id.as_ref())
            })
            .await?;
        if response.status() == 405 {
            return Ok(());
        }
        response
            .error_for_status()
            .map(|_| ())
            .map_err(|e| StreamableHttpError::Client(Failure::Http(e)))
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth: Option<String>,
        custom: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<sse_stream::Sse, SseError>>, HttpError> {
        let (response, _) = self
            .exchange(&uri, &custom, false, |headers| {
                let mut request = self
                    .client
                    .get(uri.as_ref())
                    .header(
                        reqwest::header::ACCEPT,
                        format!("{EVENT_STREAM}, {JSON_TYPE}"),
                    )
                    .headers(headers);
                if let Some(session) = &session_id {
                    request = request.header(SESSION_HEADER, session.as_ref());
                }
                if let Some(last) = &last_event_id {
                    request = request.header("last-event-id", last.as_str());
                }
                request
            })
            .await?;
        if response.status() == 405 {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = response
            .error_for_status()
            .map_err(|e| StreamableHttpError::Client(Failure::Http(e)))?;
        Ok(sse(response, MAX_SSE_EVENT_BYTES))
    }
}
