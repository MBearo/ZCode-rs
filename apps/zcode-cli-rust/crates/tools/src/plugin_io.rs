//! Network and Git ports of plugin management (Node
//! `createNodeWebFetchHttpClientAdapter`, `buildMarketplaceGitEnv`). Spec
//! rust-m10-4-plugin-sources §4.3, §6.
use anyhow::{Result, bail};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zcode_cli_net::{Egress, NetworkPolicy, Purpose};
use zcode_cli_plugins::git::Git;
use zcode_cli_plugins::http::{Get, Http, Response};

/// Node `AbortSignal` reason text of a cancelled fetch.
const ABORTED: &str = "This operation was aborted";

pub(super) struct PluginHttp {
    pub(super) egress: Arc<Egress>,
}

fn request_error(error: &reqwest::Error, timeout: std::time::Duration) -> anyhow::Error {
    if error.is_timeout() {
        anyhow::anyhow!("HTTP request timed out after {}ms", timeout.as_millis())
    } else {
        // undici 的通用网络错误文案。
        anyhow::anyhow!("fetch failed")
    }
}

#[async_trait::async_trait]
impl Http for PluginHttp {
    async fn get(&self, request: Get<'_>, cancel: &CancellationToken) -> Result<Response> {
        let client = self.egress.client(Purpose::WebFetch).await?;
        let mut builder = client.get(request.url).timeout(request.timeout);
        for (key, value) in request.headers {
            builder = builder.header(key, value);
        }
        let (max, timeout) = (request.max_bytes, request.timeout);
        let exchange = async {
            let mut response = builder
                .send()
                .await
                .map_err(|e| request_error(&e, timeout))?;
            let status = response.status();
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if let Some(length) = response.content_length().filter(|n| *n > max) {
                bail!("HTTP response is too large: content-length={length}, max={max}");
            }
            let mut body = vec![];
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| request_error(&e, timeout))?
            {
                if (body.len() + chunk.len()) as u64 > max {
                    bail!("HTTP response is too large: bytes>{max}");
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Response {
                status: status.as_u16(),
                status_text: status.canonical_reason().unwrap_or_default().to_owned(),
                location,
                body,
            })
        };
        tokio::select! {
            _ = cancel.cancelled() => bail!(ABORTED),
            result = exchange => result,
        }
    }
}

/// Node `buildMarketplaceGitEnv`: the sanitized process environment plus
/// the ZCode egress variables (the config file's `network` section is not read).
pub(super) fn git(egress: &Egress) -> Git {
    let env = egress.runtime_env();
    let binary = env
        .get("ZCODE_GIT_BINARY")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .unwrap_or("git")
        .to_owned();
    Git {
        binary,
        env: zcode_cli_net::tool_env(env.vars(), &NetworkPolicy::default(), cfg!(windows)),
    }
}

/// Node `process.env.HOME` (empty when unset).
pub(super) fn home(egress: &Egress) -> String {
    egress
        .runtime_env()
        .get("HOME")
        .unwrap_or_default()
        .to_owned()
}
