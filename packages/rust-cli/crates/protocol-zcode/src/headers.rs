//! 账号模型的每请求鉴权（缺口 G8）：平台层注入的 [`HeaderSource`] 由这里实现，照 Node 的
//! `provider-runtime-headers.ts`，经反向请求 `interaction/requestProviderRuntimeHeaders` 问 Host 要这次请求的凭据；
//! 调用方放弃（模型请求被取消）或超过 180 s 时发 `interaction/providerRuntimeHeadersCancelled`，Host 释放它的队列。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_platform::header_source::{HeaderRequest, HeaderSource, SuppliedAuth};
use serde_json::{Value, json};

use crate::context::Context;
use crate::inbound::Inbound;

pub const REQUEST: &str = "interaction/requestProviderRuntimeHeaders";
pub const CANCELLED: &str = "interaction/providerRuntimeHeadersCancelled";
/// 端口的总时限（Node `PROVIDER_RUNTIME_HEADERS_TIMEOUT_MS`）：同时覆盖 Host 的排队与凭据解析。
const LIMIT: Duration = Duration::from_secs(180);

/// 经 Host 取账号凭据。
pub struct HostHeaders {
    context: Arc<Context>,
}

impl HostHeaders {
    pub fn new(context: Arc<Context>) -> Self {
        Self { context }
    }
}

/// 请求还没得到回复就被丢弃时，告诉 Host 取消。
struct Pending {
    context: Arc<Context>,
    params: Option<Value>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(params) = self.params.take() {
            self.context.peer.try_notify(CANCELLED, &params);
        }
    }
}

fn failed(message: impl Into<String>) -> ModelError {
    ModelError::Authentication {
        message: message.into(),
    }
}

#[async_trait]
impl HeaderSource for HostHeaders {
    async fn request_auth(&self, request: HeaderRequest) -> Result<SuppliedAuth, ModelError> {
        let session = request
            .session
            .as_ref()
            .ok_or_else(|| failed("account credentials need the requesting session"))?;
        let workspace = self
            .context
            .workspace_of(session.as_str())
            .ok_or_else(|| failed("the requesting session is not open in this agent"))?;
        let request_id = format!(
            "{}:provider-runtime-headers:{}",
            session.as_str(),
            self.context.fresh("auth")
        );
        let workspace = json!({"workspacePath": workspace, "workspaceKey": workspace});
        let mut params = json!({
            "requestId": request_id,
            "sessionId": session.as_str(),
            "workspace": workspace,
            "modelSelection": {"providerId": request.selection.provider, "modelId": request.selection.model},
            "providerId": request.selection.provider,
            "accountAccess": {"type": "zhipu-account", "accountType": request.account_type,
                "mode": request.mode, "entitled": true},
            "reason": "model-request",
        });
        if let Some(fingerprint) = &request.rejected_fingerprint {
            params["rejectedProjectTokenFingerprint"] = json!(fingerprint);
        }
        let mut pending = Pending {
            context: self.context.clone(),
            params: Some(
                json!({"requestId": request_id, "sessionId": session.as_str(),
                "workspace": workspace}),
            ),
        };
        let answer = tokio::time::timeout(LIMIT, self.context.peer.request(REQUEST, &params)).await;
        let result = match answer {
            // 超时：Pending 的 Drop 发取消。
            Err(_) => return Err(failed("Requesting the account credentials timed out")),
            Ok(result) => {
                pending.params = None;
                result.map_err(|error| failed(error.message))?
            }
        };
        Inbound::get()
            .reply(REQUEST, &result)
            .map_err(|error| failed(format!("invalid account credentials reply: {error}")))?;
        if result["headersApplied"] != true {
            return Err(failed(result["errorMessage"].as_str().unwrap_or(
                "Provider runtime headers were not applied before model request attempt.",
            )));
        }
        let auth = &result["requestAuth"];
        let headers = auth["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| {
                        Some((name.clone(), RedactedString::new(value.as_str()?)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SuppliedAuth {
            api_key: auth["apiKey"].as_str().map(RedactedString::new),
            headers,
        })
    }
}

#[cfg(test)]
#[path = "headers_tests.rs"]
mod tests;
