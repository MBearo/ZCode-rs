use super::{
    config::ModelConfig,
    model_failure,
    model_policy::RetryPolicy,
    model_protocol::{self, ApiType, ProtocolStream},
    model_stream::TextBuffer,
    network_status::{self, Attempt, Reporter},
    sse::SseDecoder,
};
use crate::contract::{Event, EventSink, ModelFailure, ModelOutput, ModelPort, RequestOrigin};
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
    async fn request(
        &self,
        (body, server_tools): (Bytes, bool),
        attempt: &mut Attempt,
        output: &mut TextBuffer<'_>,
        auth: &Value,
        reporter: &Reporter<'_>,
    ) -> Result<(ModelOutput, serde_json::Map<String, Value>)> {
        let idle_ms = self.idle_ms(attempt.budget());
        // Node applyModelRequestAuth：请求级鉴权的 apiKey 覆盖任意 provider 的配置 key。
        let key = match auth["requestAuth"]["apiKey"].as_str() {
            Some(key) => Some(key.to_owned()),
            None if self.config.account_access.is_some() => None,
            None => self
                .config
                .api_key()
                .map_err(|_| ModelFailure::new("auth_failed", false))?,
        };
        let mut headers = self.headers(key.as_deref(), auth, &output.origin(), attempt)?;
        if server_tools {
            // AI SDK webSearch_20260209 的 beta；已有 beta 时逗号追加。
            const BETA: &str = "code-execution-web-tools-2026-02-09";
            let merged = match headers.get("anthropic-beta") {
                Some(existing) if existing.split(',').any(|b| b.trim() == BETA) => {
                    existing.to_owned()
                }
                Some(existing) => format!("{existing},{BETA}"),
                None => BETA.to_owned(),
            };
            headers.set("anthropic-beta", merged);
        }
        attempt.phase = "stream";
        output.status(reporter.started(attempt)).await?;
        let mut request = self.client().await?.post(&self.url).body(body);
        for (name, value) in headers.iter() {
            request = request.header(name, value);
        }
        if let Some(seconds) = self.config.request_timeout_seconds {
            request = request.timeout(Duration::from_secs(seconds));
        }
        let response = tokio::select! {
            result=request.send()=>result.map_err(|e| model_failure::network(&e))?,
            _=deadline(after(idle_ms))=>return Err(self.stall(output, reporter, attempt).await),
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            loop {
                let chunk = tokio::select! {
                    chunk=stream.next()=>chunk,
                    _=deadline(after(idle_ms))=>return Err(self.stall(output, reporter, attempt).await),
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
            let mut failure = model_failure::response(Some(status), &body, &headers);
            let headers = network_status::response_headers(&headers);
            failure.detail = Some(Box::new(network_status::provider_detail(&body, headers)));
            return Err(failure);
        }
        let response_headers = network_status::response_headers(response.headers());
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut assembly = ProtocolStream::new(self.config.api_type, server_tools);
        let mut idle_at = after(idle_ms);
        loop {
            tokio::select! {biased;
                _=deadline(output.deadline)=> {
                    let before = Instant::now();
                    output.flush().await?;
                    // stdout 背压不算供应商闲置；不能因 UI 暂停读管道误报网络故障。
                    idle_at = idle_at.and_then(|at| at.checked_add(before.elapsed()));
                },
                _=deadline(idle_at)=>return Err(self.stall(output, reporter, attempt).await),
                chunk=stream.next()=> {
                    let Some(chunk) = chunk else { break; };
                    let chunk = chunk.map_err(|e| model_failure::network(&e))?;
                    let events = decoder.push(&chunk)?;
                    let had_event = !events.is_empty();
                    if had_event { attempt.first_event.get_or_insert_with(Instant::now); }
                    for data in events {
                        assembly.consume(&data,output).await?;
                        if assembly.done() { break; }
                    }
                    if had_event { idle_at = after(idle_ms); }
                    if assembly.done() { break; }
                },
            }
        }
        if assembly.named_call() {
            output.tool().await?;
        }
        Ok((assembly.finish()?, response_headers))
    }
    /// `current`: the in-flight attempt, reported as cancelled if the caller stops.
    async fn complete_inner(
        &self,
        messages: Vec<Value>,
        tools: &[Value],
        sink: &EventSink,
        (current, logical_call): (&std::sync::Mutex<Option<Attempt>>, &str),
    ) -> Result<ModelOutput> {
        let mut messages = messages;
        let has_attachments =
            super::request_attachments::materialize(&mut messages, &self.format_properties())
                .await?;
        let user = match self.config.api_type {
            ApiType::Anthropic => Some(self.anthropic_user(&sink.origin).await),
            _ => None,
        };
        let server_tools = tools.iter().any(|t| t["type"] != "function");
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
        let reporter = Reporter {
            config: &self.config,
            origin: &sink.origin,
            max_attempts: self.retry.max_attempts,
            logical_call,
        };
        let mut empty_retries = 0;
        // 断流恢复的新请求在适配层是 attempt 1；空闲超时按恢复次数继续递增（Node streamIdleTimeoutRetryNumber）。
        let recovery = sink
            .origin
            .stream_recovery
            .as_ref()
            .and_then(|r| r["retryNumber"].as_u64())
            .unwrap_or(0) as u32;
        for number in 1..=self.retry.max_attempts {
            let mut attempt = Attempt::new(number);
            attempt.recovery = recovery;
            *current.lock().unwrap() = Some(attempt.clone());
            let mut output = TextBuffer::new(sink);
            let result = match self.request_auth(sink).await {
                Ok(auth) => {
                    let result = self
                        .request(
                            (encoded.clone(), server_tools),
                            &mut attempt,
                            &mut output,
                            &auth,
                            &reporter,
                        )
                        .await;
                    *current.lock().unwrap() = Some(attempt.clone());
                    result
                }
                Err(failure) => Err(failure),
            };
            output.flush().await?;
            let timings = (output.first_content, output.first_text);
            match result {
                Ok((mut result, response)) => {
                    let status = reporter.completed(
                        &attempt,
                        Some(&result),
                        &response,
                        timings,
                        output.committed,
                    );
                    *current.lock().unwrap() = None;
                    output.status(status).await?;
                    result.message["_zcode_origin"] = serde_json::json!({"provider":self.config.provider_id,"model":self.config.model_id});
                    return Ok(result);
                }
                Err(mut failure) => {
                    failure.output_committed = output.committed;
                    let idle_ms = self.idle_ms(attempt.budget());
                    let retry = failure.retryable
                        && !failure.output_committed
                        && number < self.retry.max_attempts
                        && !(failure.empty_completion && empty_retries > 0);
                    if !retry {
                        *current.lock().unwrap() = None;
                        // Node：终止的空响应在 adapter 层报 completed，由 core 抛出错误。
                        let status = if failure.empty_completion {
                            reporter.completed(&attempt, None, &Default::default(), timings, false)
                        } else {
                            reporter.failed(&attempt, &failure, false, idle_ms)
                        };
                        output.status(status).await?;
                        return Err(failure);
                    }
                    output
                        .status(reporter.failed(&attempt, &failure, true, idle_ms))
                        .await?;
                    if failure.empty_completion {
                        empty_retries += 1;
                    }
                    let mask = (1u64 << 53) - 1;
                    let random =
                        (uuid::Uuid::new_v4().as_u128() as u64 & mask) as f64 / mask as f64;
                    let delay_ms = self.retry.delay_ms(number, failure.retry_after_ms, random);
                    output
                        .status(reporter.retry(&attempt, &failure, delay_ms, idle_ms))
                        .await?;
                    // 退避期间取消：Node 以同一次尝试报 connect 阶段的 cancelled。
                    attempt.phase = "connect";
                    *current.lock().unwrap() = Some(attempt);
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
    fn auxiliary(&self) -> Option<Arc<dyn ModelPort>> {
        self.with_max_output_tokens(4096).ok().flatten()
    }
    fn supports_native_web_search(&self) -> bool {
        self.config.supports_native_web_search
    }
    fn account_auth(&self) -> bool {
        self.config.account_access.is_some()
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
        let current = std::sync::Mutex::new(None);
        // Node 每次模型调用（含其重试）共用一个 logicalCallId。
        let logical_call = uuid::Uuid::new_v4().to_string();
        let result = tokio::select! {biased;
            _=cancel.cancelled()=>Err(ModelFailure::cancelled()),
            result=self.complete_inner(messages,tools,sink,(&current,&logical_call))=>return result,
        };
        let attempt = current.lock().unwrap().take();
        if let Some(attempt) = attempt {
            let reporter = Reporter {
                config: &self.config,
                origin: &sink.origin,
                max_attempts: self.retry.max_attempts,
                logical_call: &logical_call,
            };
            let failure = ModelFailure::cancelled();
            let status = reporter.failed(&attempt, &failure, false, self.idle_ms(attempt.budget()));
            let _ = sink.send(Event::ModelStatus(status)).await;
        }
        result
    }
}
#[path = "provider_auth.rs"]
mod auth;
#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
