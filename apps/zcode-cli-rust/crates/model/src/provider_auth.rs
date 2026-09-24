//! Per-attempt request preparation of [`HttpModel`]: headers, the Host
//! credential round-trip and the idle stall report.
use super::*;

impl HttpModel {
    /// Node header order: SDK auth < identity < OpenRouter < `api.headers` <
    /// `requestAuth.headers` < per-request attribution, merged case-insensitively.
    /// Also records the attempt's sanitized status view: everything but the
    /// SDK-level content type, accept, version and key-derived auth headers.
    pub(super) fn headers(
        &self,
        key: Option<&str>,
        auth: &Value,
        origin: &RequestOrigin,
        attempt: &mut Attempt,
    ) -> Result<Headers> {
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
        let attribution = headers::attribution(&headers::Attribution {
            request_id: &attempt.request_id,
            session_type: origin.kind.as_str(),
            trace_id: &origin.trace_id,
            query_id: origin.query_id.as_deref(),
            session_id: origin.session_id.as_deref(),
            base_url: &self.config.base_url,
        });
        headers.extend(attribution.iter());
        attempt.request_headers =
            network_status::sanitize(resolved.iter().chain(attribution.iter()));
        if self.via_gateway {
            // 显式 Host 指向官方端点主机；改走网关后由客户端按实际 URL 计算。
            headers.remove("host");
        }
        Ok(headers)
    }
    /// Credentials for one attempt: the execution's frozen `requestAuth`, a
    /// Host round-trip for account providers, or none.
    pub(super) async fn request_auth(&self, sink: &EventSink) -> Result<Value> {
        if let Some(frozen) = &sink.request_auth {
            // 本轮冻结鉴权（modelExecution.requestAuth）直接生效，不向 Host 请求。
            return Ok(serde_json::json!({"headersApplied":true,"requestAuth":frozen.0}));
        }
        let Some(access) = &self.config.account_access else {
            return Ok(Value::Null);
        };
        let (reply, received) = tokio::sync::oneshot::channel();
        sink.send(Event::RequestAuth {
            provider: self.config.provider_id.clone(),
            selection: serde_json::json!({"providerId":self.config.provider_id,"modelId":self.config.model_id,"options":{"reasoningLevel":self.config.reasoning_level}}),
            access: access.clone(),
            reply,
        })
        .await
        .map_err(|_| ModelFailure::cancelled())?;
        let auth = tokio::time::timeout(Duration::from_secs(180), received)
            .await
            .map_err(|_| ModelFailure::new("auth_failed", false))?
            .map_err(|_| ModelFailure::cancelled())?;
        if auth["headersApplied"] != true || !auth["requestAuth"].is_object() {
            return Err(ModelFailure::new("auth_failed", false));
        }
        Ok(auth)
    }

    /// Stream idle timeout of an attempt: 30 s longer per retry; 0 disables it.
    pub(super) fn idle_ms(&self, attempt: u32) -> u64 {
        if self.config.stream_idle_timeout_ms == 0 {
            return 0;
        }
        self.config
            .stream_idle_timeout_ms
            .saturating_add(u64::from(attempt - 1) * 30_000)
    }

    /// Node `model_stream_stalled`, then the attempt fails as `stream_idle_timeout`.
    pub(super) async fn stall(
        &self,
        output: &TextBuffer<'_>,
        reporter: &Reporter<'_>,
        attempt: &Attempt,
    ) -> ModelFailure {
        let status = reporter.stalled(attempt, self.idle_ms(attempt.number));
        match output.status(status).await {
            Ok(()) => ModelFailure::new("stream_idle_timeout", true),
            Err(cancelled) => cancelled,
        }
    }
}
