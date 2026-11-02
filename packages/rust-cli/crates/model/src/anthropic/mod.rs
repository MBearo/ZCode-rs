//! Anthropic Messages 协议（docs/specs/rust-cli/model.md §3）：URL、header 与四个部件的组装。

use rust_cli_kernel_api::model::{ModelError, ModelRequest};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint};
use rust_cli_net::{HeaderValue, HttpRequest, header};

use crate::endpoint::anthropic_messages_url;
use crate::errors::Vendor;
use crate::profiles::Profile;
use crate::protocol::{Protocol, StreamDecoder};
use crate::wire::HEADER_TIMEOUT;

pub(crate) mod betas;
mod output;
pub(crate) mod request;
mod shape;
pub(crate) mod stream;
mod system_fold;
mod write;

use stream::AnthropicStream;

/// SDK 的 `anthropic-version`。
pub(crate) const API_VERSION: &str = "2023-06-01";
/// Anthropic 的版本头与 beta 头（消息与计数两种请求都发，F55：以前各写一份）。
pub(crate) const VERSION_HEADER: &str = "anthropic-version";
pub(crate) const BETA_HEADER: &str = "anthropic-beta";

pub(crate) struct Anthropic;

impl Protocol for Anthropic {
    fn encode(
        &self,
        endpoint: &ModelEndpoint,
        mut request: ModelRequest,
        profile: &Profile,
    ) -> Result<HttpRequest, ModelError> {
        shape::apply(&mut request, profile);
        if !endpoint.capabilities.supports_mid_conversation_system {
            system_fold::fold(&mut request);
        }
        request.selection.model.clone_from(&endpoint.model);
        let explicit = std::mem::take(&mut request.betas);
        let encoded = request::encode(request, profile, &endpoint.body_patches, &explicit)?;
        // 兼容端点（方案不带一方的专有项）照 Node 发 beta（model.md §3.1 的“兼容端点”）。修 bug（F64）：以前从按特性的
        // 列表里只留两项，对话中间有 system 消息时少发 `mid-conversation-system`（Node 发）。
        let betas = if profile.first_party_extras {
            encoded.betas
        } else if crate::profiles::is_gateway(&endpoint.base_url) {
            // 用户决定（2026-10-23，F71）：经网关不发 `structured-outputs-2025-11-13`（转到 Bedrock 等后端的网关可能
            // 拒绝它），其余照 Node。
            let mut betas = encoded.node_betas;
            betas.retain(|beta| beta != betas::NODE_STRUCTURED_OUTPUTS);
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
                vec![(
                    header::X_API_KEY.to_owned(),
                    HeaderValue::Secret(key.clone()),
                )]
            }
        }
    }

    fn decoder(&self, vendor: Vendor) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicStream::new(vendor))
    }
}

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "protocol_parity_tests.rs"]
mod parity_tests;
