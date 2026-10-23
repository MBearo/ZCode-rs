//! http hook 的出口（docs/specs/rust-cli/hooks.md §5.2）：kernel-api `HookHttp` 端口的本地实现，
//! 请求经 `rust-cli-net` 的 [`Egress`] 发出。header 的插值与 URL 允许表由 hooks crate 处理；这里只发送，
//! 超时覆盖整个请求。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::hooks::{HookHttp, HookHttpError, HookHttpRequest, HookHttpResponse};
use rust_cli_net::{Egress, HeaderValue, HttpRequest, NetError};
use tokio_util::sync::CancellationToken;

/// 响应体最多读这么多字节。
const BODY_LIMIT: usize = 10 * 1024 * 1024;

pub struct LocalHookHttp {
    egress: Arc<Egress>,
}

impl LocalHookHttp {
    pub fn new(egress: Arc<Egress>) -> Self {
        Self { egress }
    }
}

fn failed(error: &NetError) -> HookHttpError {
    match error {
        NetError::Timeout => HookHttpError::TimedOut,
        other => HookHttpError::Failed {
            message: other.to_string(),
        },
    }
}

/// 出站请求。
///
/// 缺陷修复 R2-13：header 原来按普通值交给 net；插值后的值常带 token（`Bearer $MY_TOKEN`），现在一律是秘密值
/// （发送时标为敏感，任何 Debug 输出都看不到原值）。
fn outbound(request: HookHttpRequest) -> HttpRequest {
    request.headers.into_iter().fold(
        HttpRequest::post(request.url, request.body),
        |http, (name, value)| http.header(name, HeaderValue::Secret(RedactedString::new(value))),
    )
}

#[async_trait]
impl HookHttp for LocalHookHttp {
    async fn post(
        &self,
        request: HookHttpRequest,
        cancel: CancellationToken,
    ) -> Result<HookHttpResponse, HookHttpError> {
        let limit = Duration::from_millis(request.timeout_ms);
        let http = outbound(request);
        let send = async {
            let response = self.egress.send(http).await.map_err(|e| failed(&e))?;
            let status = response.status();
            let body = response.text(BODY_LIMIT).await.map_err(|e| failed(&e))?;
            Ok(HookHttpResponse { status, body })
        };
        tokio::select! {
            sent = tokio::time::timeout(limit, send) => sent.unwrap_or(Err(HookHttpError::TimedOut)),
            () = cancel.cancelled() => Err(HookHttpError::Cancelled),
        }
    }
}

#[cfg(test)]
#[path = "hook_http_tests.rs"]
mod tests;
