//! Anthropic Messages 协议（docs/specs/rust-cli/model.md §3）：URL、header 与四个部件的组装。

use std::time::{Duration, SystemTime};

use rust_cli_kernel_api::model::{ModelError, ModelRequest};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint};
use rust_cli_net::{HeaderValue, HttpRequest, ResponseHeaders, header};

use crate::anthropic_stream::AnthropicStream;
use crate::endpoint::anthropic_messages_url;
use crate::profiles::Profile;
use crate::protocol::{Failure, Protocol, StreamDecoder};
use crate::{anthropic_error, anthropic_request, anthropic_shape, system_fold};

/// SDK 的 `anthropic-version`。
pub(crate) const API_VERSION: &str = "2023-06-01";
/// Anthropic 的版本头与 beta 头（消息与计数两种请求都发，F55：以前各写一份）。
pub(crate) const VERSION_HEADER: &str = "anthropic-version";
pub(crate) const BETA_HEADER: &str = "anthropic-beta";
/// 等待响应头的时限：SDK 默认 timeout；值在 net 的单次超时表（F58）。
pub(crate) const HEADER_TIMEOUT: Duration = rust_cli_net::timeout::MODEL;

pub(crate) struct Anthropic;

impl Protocol for Anthropic {
    fn encode(
        &self,
        endpoint: &ModelEndpoint,
        mut request: ModelRequest,
        profile: &Profile,
    ) -> Result<HttpRequest, ModelError> {
        anthropic_shape::apply(&mut request, profile);
        if !endpoint.mid_conversation_system {
            system_fold::fold(&mut request);
        }
        request.selection.model.clone_from(&endpoint.model);
        let explicit = std::mem::take(&mut request.betas);
        let encoded =
            anthropic_request::encode(request, profile, &endpoint.body_patches, &explicit)?;
        // 兼容端点（方案不带一方的专有项）照 Node 发 beta（model.md §3.1 的“兼容端点”）。修 bug（F64）：以前从按特性的
        // 列表里只留两项，对话中间有 system 消息时少发 `mid-conversation-system`（Node 发）。
        let betas = if profile.first_party_extras {
            encoded.betas
        } else if crate::profiles::is_gateway(&endpoint.base_url) {
            // 用户决定（2026-10-23，F71）：经网关不发 `structured-outputs-2025-11-13`（转到 Bedrock 等后端的网关可能
            // 拒绝它），其余照 Node。
            let mut betas = encoded.node_betas;
            betas.retain(|beta| beta != crate::anthropic_betas::NODE_STRUCTURED_OUTPUTS);
            betas
        } else {
            encoded.node_betas
        };
        // SDK：`betas` 数组的 toString（逗号连接）进 `anthropic-beta`；没有 beta 时不发这个 header。
        let betas = betas.join(",");
        let mut http = HttpRequest::post(anthropic_messages_url(&endpoint.base_url), encoded.body)
            .header(header::ACCEPT, header::JSON)
            .header(header::CONTENT_TYPE, header::JSON)
            .header(VERSION_HEADER, API_VERSION);
        if !betas.is_empty() {
            http = http.header(BETA_HEADER, betas);
        }
        http.timeout = Some(HEADER_TIMEOUT);
        Ok(http)
    }

    /// 只发 `x-api-key`（API key 的写法）。兼容网关需要的 Bearer 由平台层
    /// 写进 Provider header（Node 的 Provider 适配器两者都发）。
    fn credential_headers(&self, credential: &Credential) -> Vec<(String, HeaderValue)> {
        match credential {
            Credential::None => Vec::new(),
            Credential::ApiKey(key) => {
                vec![("x-api-key".to_owned(), HeaderValue::Secret(key.clone()))]
            }
        }
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicStream::default())
    }

    fn classify(
        &self,
        status: u16,
        headers: &ResponseHeaders,
        body: &str,
        now: SystemTime,
    ) -> Failure {
        anthropic_error::from_response(status, headers, body, now)
    }
}

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "anthropic_parity_tests.rs"]
mod parity_tests;
