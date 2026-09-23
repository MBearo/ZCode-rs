use super::{
    config::ModelConfig,
    model_failure,
    model_policy::RetryPolicy,
    model_protocol::{self, ApiType, ProtocolStream},
    model_stream::TextBuffer,
    sse::SseDecoder,
};
use crate::contract::{
    Event, EventSink, ModelFailure, ModelOutput, ModelPort, RequestOrigin, RetryState,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zcode_cli_net::{Egress, EgressError, Headers, Purpose, headers};

type Result<T> = std::result::Result<T, ModelFailure>;
pub struct HttpModel {
    config: ModelConfig,
    egress: Arc<Egress>,
    retry: RetryPolicy,
    /// Actual request URL; official Coding Plan endpoints already point at the gateway.
    url: String,
    via_gateway: bool,
}
impl HttpModel {
    pub fn new(config: ModelConfig, egress: Arc<Egress>) -> Self {
        let retry = RetryPolicy::resolve(&config.retry);
        let endpoint = config.api_type.url(&config.base_url);
        // 与 Node 一致：先改写官方端点，再按实际发送地址判定代理与 no_proxy。
        let (url, via_gateway) = match egress.gateway(&endpoint) {
            Some(gateway) => (gateway, true),
            None => (endpoint, false),
        };
        Self {
            config,
            egress,
            retry,
            url,
            via_gateway,
        }
    }
    async fn client(&self) -> Result<reqwest::Client> {
        self.egress
            .client(Purpose::Model)
            .await
            .map_err(|e| match e {
                EgressError::Client(e) => model_failure::network(&e),
                EgressError::CaCertificate(_) => ModelFailure::new("tls_error", false),
            })
    }
    /// Node header order: SDK auth < identity < OpenRouter < `api.headers` <
    /// `requestAuth.headers` < per-request attribution, merged case-insensitively.
    fn headers(&self, key: Option<&str>, auth: &Value, origin: &RequestOrigin) -> Result<Headers> {
        let mut resolved = self.egress.identity().clone();
        headers::with_openrouter(&mut resolved, &self.config.base_url);
        resolved.extend(
            self.config
                .headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        );
        if let Some(extra) = auth["requestAuth"]["headers"].as_object() {
            for (name, value) in extra {
                let value = value
                    .as_str()
                    .ok_or_else(|| ModelFailure::new("auth_failed", false))?;
                resolved.set(name.as_str(), value);
            }
        }
        let mut headers = Headers::default();
        headers.set("content-type", "application/json");
        headers.set("accept", "text/event-stream");
        if self.config.api_type == ApiType::Anthropic {
            headers.set("anthropic-version", "2023-06-01");
            if let Some(key) = key {
                headers.set("x-api-key", key);
                // Anthropic 兼容网关同时读取 Bearer；显式配置的 Authorization 优先。
                if !resolved.contains("authorization") {
                    headers.set("Authorization", format!("Bearer {key}"));
                }
            }
        } else if let Some(key) = key {
            headers.set("Authorization", format!("Bearer {key}"));
        }
        headers.extend(resolved.iter());
        let request_id = uuid::Uuid::new_v4().to_string();
        headers.extend(
            headers::attribution(&headers::Attribution {
                request_id: &request_id,
                session_type: origin.kind.as_str(),
                trace_id: &origin.trace_id,
                query_id: origin.query_id.as_deref(),
                session_id: origin.session_id.as_deref(),
                base_url: &self.config.base_url,
            })
            .iter(),
        );
        if self.via_gateway {
            // 显式 Host 指向官方端点主机；改走网关后由客户端按实际 URL 计算。
            headers.remove("host");
        }
        Ok(headers)
    }
    async fn request(
        &self,
        body: Bytes,
        attempt: u32,
        output: &mut TextBuffer<'_>,
        auth: &Value,
    ) -> Result<ModelOutput> {
        let idle_ms = if self.config.stream_idle_timeout_ms == 0 {
            0
        } else {
            self.config
                .stream_idle_timeout_ms
                .saturating_add(u64::from(attempt - 1) * 30_000)
        };
        let key = if self.config.account_access.is_some() {
            auth["requestAuth"]["apiKey"].as_str().map(str::to_owned)
        } else {
            self.config
                .api_key()
                .map_err(|_| ModelFailure::new("auth_failed", false))?
        };
        let headers = self.headers(key.as_deref(), auth, &output.origin())?;
        let mut request = self.client().await?.post(&self.url).body(body);
        for (name, value) in headers.iter() {
            request = request.header(name, value);
        }
        if let Some(seconds) = self.config.request_timeout_seconds {
            request = request.timeout(Duration::from_secs(seconds));
        }
        let response = tokio::select! {
            result=request.send()=>result.map_err(|e| model_failure::network(&e))?,
            _=deadline(after(idle_ms))=>return Err(ModelFailure::new("stream_idle_timeout",true)),
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            loop {
                let chunk = tokio::select! {
                    chunk=stream.next()=>chunk,
                    _=deadline(after(idle_ms))=>return Err(ModelFailure::new("stream_idle_timeout",true)),
                };
                let Some(chunk) = chunk else {
                    break;
                };
                let chunk = chunk.map_err(|e| model_failure::network(&e))?;
                if bytes.len() + chunk.len() > 65536 {
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(model_failure::response(Some(status), &body, &headers));
        }
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut assembly = ProtocolStream::new(self.config.api_type);
        let mut idle_at = after(idle_ms);
        loop {
            tokio::select! {biased;
                _=deadline(output.deadline)=> {
                    let before = Instant::now();
                    output.flush().await?;
                    // stdout 背压不算供应商闲置；不能因 UI 暂停读管道误报网络故障。
                    idle_at = idle_at.and_then(|at| at.checked_add(before.elapsed()));
                },
                _=deadline(idle_at)=>return Err(ModelFailure::new("stream_idle_timeout",true)),
                chunk=stream.next()=> {
                    let Some(chunk) = chunk else { break; };
                    let chunk = chunk.map_err(|e| model_failure::network(&e))?;
                    let events = decoder.push(&chunk)?;
                    let had_event = !events.is_empty();
                    for data in events {
                        assembly.consume(&data,output).await?;
                        if assembly.done() { break; }
                    }
                    if had_event { idle_at = after(idle_ms); }
                    if assembly.done() { break; }
                },
            }
        }
        assembly.finish()
    }
    async fn complete_inner(
        &self,
        messages: Vec<Value>,
        tools: &[Value],
        sink: &EventSink,
    ) -> Result<ModelOutput> {
        let mut messages = messages;
        let has_attachments =
            super::request_attachments::materialize(&mut messages, &self.format_properties())
                .await?;
        let user = match self.config.api_type {
            ApiType::Anthropic => Some(self.anthropic_user(&sink.origin).await),
            _ => None,
        };
        let body = model_protocol::body(&self.config, messages, tools, user.as_deref())?;
        // Bytes 克隆只增加引用计数；同一模型步骤的网络重试不再编码整段历史。
        let encoded = Bytes::from(
            serde_json::to_vec(&body).map_err(|_| ModelFailure::new("invalid_request", false))?,
        );
        // 大附件仅保留重试所需的已编码字节，不能在整个流期间保留多份 base64 请求树。
        drop(body);
        if encoded.len()
            > if has_attachments {
                96 * 1024 * 1024
            } else {
                2 * 1024 * 1024
            }
        {
            return Err(ModelFailure::new("context_exceeded", false));
        }
        let mut empty_retries = 0;
        for attempt in 1..=self.retry.max_attempts {
            if attempt > 1 {
                sink.send(Event::Retry(None))
                    .await
                    .map_err(|_| ModelFailure::cancelled())?;
            }
            let mut output = TextBuffer::new(sink);
            let auth = if let Some(access) = &self.config.account_access {
                let (reply, received) = tokio::sync::oneshot::channel();
                sink.send(Event::RequestAuth {
                    provider: self.config.provider_id.clone(),
                    selection: serde_json::json!({"providerId":self.config.provider_id,"modelId":self.config.model_id,"options":{"reasoningLevel":self.config.reasoning_level}}),
                    access: access.clone(), reply,
                }).await.map_err(|_| ModelFailure::cancelled())?;
                let auth = tokio::time::timeout(Duration::from_secs(180), received)
                    .await
                    .map_err(|_| ModelFailure::new("auth_failed", false))?
                    .map_err(|_| ModelFailure::cancelled())?;
                if auth["headersApplied"] != true || !auth["requestAuth"].is_object() {
                    return Err(ModelFailure::new("auth_failed", false));
                }
                auth
            } else {
                Value::Null
            };
            let result = self
                .request(encoded.clone(), attempt, &mut output, &auth)
                .await;
            output.flush().await?;
            match result {
                Ok(mut result) => {
                    result.message["_zcode_origin"] = serde_json::json!({"provider":self.config.provider_id,"model":self.config.model_id});
                    return Ok(result);
                }
                Err(mut failure) => {
                    failure.output_committed = output.committed;
                    if !failure.retryable
                        || failure.output_committed
                        || attempt == self.retry.max_attempts
                        || (failure.empty_completion && empty_retries > 0)
                    {
                        return Err(failure);
                    }
                    if failure.empty_completion {
                        empty_retries += 1;
                    }
                    let mask = (1u64 << 53) - 1;
                    let random =
                        (uuid::Uuid::new_v4().as_u128() as u64 & mask) as f64 / mask as f64;
                    let delay_ms = self.retry.delay_ms(attempt, failure.retry_after_ms, random);
                    let reason = if failure.empty_completion {
                        "server_error"
                    } else {
                        failure.reason
                    };
                    sink.send(Event::Retry(Some(RetryState {
                        attempt,
                        max_attempts: self.retry.max_attempts,
                        next_retry_at: super::now().saturating_add(delay_ms),
                        reason_code: reason,
                    })))
                    .await
                    .map_err(|_| ModelFailure::cancelled())?;
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
        unreachable!("positive retry budget")
    }
}
impl HttpModel {
    /// Node `resolveAnthropicRequestMetadataUserId`; key order is part of the value.
    async fn anthropic_user(&self, origin: &RequestOrigin) -> String {
        let session = headers::session_for_attribution(origin.session_id.as_deref());
        format!(
            r#"{{"device_id":{},"account_uuid":"","session_id":{}}}"#,
            Value::from(self.egress.device_id().await),
            Value::from(session.unwrap_or_default())
        )
    }
}
fn after(ms: u64) -> Option<Instant> {
    if ms == 0 {
        None
    } else {
        Instant::now().checked_add(Duration::from_millis(ms))
    }
}
async fn deadline(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
#[async_trait::async_trait]
impl ModelPort for HttpModel {
    fn identity(&self) -> Option<crate::contract::ModelIdentity> {
        Some(crate::contract::ModelIdentity {
            provider_id: self.config.provider_id.clone(),
            model_id: self.config.model_id.clone(),
            reasoning_level: self.config.reasoning_level.clone(),
        })
    }
    fn format_properties(&self) -> Value {
        self.config.format_properties.clone().unwrap_or_else(|| serde_json::json!({"inputFormat":{"supportsText":true,"supportsImage":false,"supportsVideo":false,"supportsAudio":false,"supportsPdf":false},"outputFormat":{"supportsText":true}}))
    }
    fn with_max_output_tokens(
        &self,
        max: usize,
    ) -> anyhow::Result<Option<std::sync::Arc<dyn ModelPort>>> {
        anyhow::ensure!(max > 0, "Invalid output token limit");
        let mut config = self.config.clone();
        config.max_output_tokens = max.min(config.max_output_tokens);
        if let Some(map) = &config.max_output_map {
            let patch = crate::domain::option_map::evaluate(
                map,
                "maxOutputTokens",
                &serde_json::json!(config.max_output_tokens),
            )?;
            config.option_patches[1] = patch;
            crate::domain::option_map::validate_patches(&config.option_patches)?;
        }
        Ok(Some(Arc::new(Self::new(config, self.egress.clone()))))
    }
    fn context_policy(&self) -> crate::domain::context::ContextPolicy {
        crate::domain::context::ContextPolicy {
            window: self.config.context_window,
            max_output: self.config.max_output_tokens,
            buffer: self.config.context_buffer_tokens,
            automatic: self.config.auto_compact,
        }
    }
    async fn complete(
        &self,
        messages: Vec<Value>,
        tools: &[Value],
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> Result<ModelOutput> {
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(ModelFailure::cancelled()),
            result=self.complete_inner(messages,tools,sink)=>result,
        }
    }
}
#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
