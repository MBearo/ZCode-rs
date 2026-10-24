//! 端点的来源与 URL 规则（docs/specs/rust-cli/model.md §1、§6）。
//!
//! 模型端口每次请求经 [`ModelEndpoints`] 取一次端点：平台层按 Provider registry 与账号给出；显式覆盖
//! （`ZCODE_MODEL_BASE_URL` / `ZCODE_API_KEY`，修复线 F42）与测试用这里的 [`FixedEndpoint`]。

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use std::sync::Arc;

use rust_cli_kernel_api::provider::{
    Credential, ModelCapabilities, ModelEndpoint, ModelEndpoints, WireProtocol,
};

/// 显式给出的 Anthropic 端点（入口从 `ZCODE_MODEL_BASE_URL` / `ZCODE_API_KEY` 构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicEndpoint {
    pub base_url: String,
    pub api_key: RedactedString,
}

/// Anthropic 官方 API 的 origin（修复线 F58：两种写法只从这一处拼出，`concat!` 用宏）。
macro_rules! anthropic_origin {
    () => {
        "https://api.anthropic.com"
    };
}
pub(crate) use anthropic_origin;

impl AnthropicEndpoint {
    /// 显式覆盖没给地址时的缺省：SDK 的写法，不带 `/v1`（同 `@anthropic-ai/sdk`）。
    pub const DEFAULT_BASE_URL: &'static str = anthropic_origin!();
}

impl From<AnthropicEndpoint> for ModelEndpoint {
    fn from(endpoint: AnthropicEndpoint) -> Self {
        Self {
            protocol: WireProtocol::AnthropicMessages,
            base_url: endpoint.base_url,
            model: String::new(),
            credential: Credential::ApiKey(endpoint.api_key),
            headers: Vec::new(),
            body_patches: Vec::new(),
            // 没有能力来源时（测试）按缺省的语义：对话中间的 system 消息原样发出，模型接受图片
            // 与 PDF。入口给了能力来源（[`FixedEndpoint::with_capabilities`]）时每次按模型改写。
            mid_conversation_system: true,
            supports_image: true,
            supports_pdf: true,
            supports_video: false,
            supports_audio: false,
            renewable: false,
        }
    }
}

/// 按模型 ID 给出能力（显式覆盖：入口用内置配置的通用规则算，docs/specs/rust-cli/model.md §6、§11）。
pub type CapabilityRules = Arc<dyn Fn(&str) -> ModelCapabilities + Send + Sync>;

/// 固定端点：每次请求都给同一份，请求体的 `model` 取选择里的模型。
pub struct FixedEndpoint {
    template: ModelEndpoint,
    capabilities: Option<CapabilityRules>,
}

impl FixedEndpoint {
    pub fn new(template: impl Into<ModelEndpoint>) -> Self {
        Self {
            template: template.into(),
            capabilities: None,
        }
    }

    /// 能力来源（修复线 F23）：端点上的能力项与 [`ModelEndpoints::capabilities`] 都按选择里的模型从它取，
    /// 不再写死（`/model` 可以换模型）。
    pub fn with_capabilities(mut self, rules: CapabilityRules) -> Self {
        self.capabilities = Some(rules);
        self
    }
}

#[async_trait]
impl ModelEndpoints for FixedEndpoint {
    async fn endpoint(
        &self,
        selection: &ModelSelection,
        _rejected: Option<&ModelEndpoint>,
    ) -> Result<ModelEndpoint, ModelError> {
        let mut endpoint = self.template.clone();
        endpoint.model.clone_from(&selection.model);
        if let Some(rules) = &self.capabilities {
            let capabilities = rules(&selection.model);
            endpoint.mid_conversation_system = capabilities.supports_mid_conversation_system;
            endpoint.supports_image = capabilities.supports_image;
            endpoint.supports_pdf = capabilities.supports_pdf;
            endpoint.supports_video = capabilities.supports_video;
            endpoint.supports_audio = capabilities.supports_audio;
        }
        Ok(endpoint)
    }

    async fn capabilities(&self, selection: &ModelSelection) -> Option<ModelCapabilities> {
        self.capabilities
            .as_ref()
            .map(|rules| rules(&selection.model))
    }
}

/// 去掉首尾空白与末尾的 `/`。
fn trimmed(base_url: &str) -> &str {
    base_url.trim().trim_end_matches('/')
}

/// 基址补 `/v1`，已以 `/v1` 结尾时不再补（Node `normalizeAnthropicBaseURL`）。请求地址与入口查能力规则用的
/// 写法（Node 的 Anthropic provider 写带 `/v1` 的地址）都从这里来（F55：以前 cli 另写了一份同样的规则）。
pub fn anthropic_v1_base_url(base_url: &str) -> String {
    let base = trimmed(base_url);
    if base.to_ascii_lowercase().ends_with("/v1") {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

/// `{base}/v1/messages?beta=true`（[`anthropic_v1_base_url`]）。用 SDK 的 beta 接口时，路径
/// 带 `?beta=true`。
pub(crate) fn anthropic_messages_url(base_url: &str) -> String {
    format!("{}/messages?beta=true", anthropic_v1_base_url(base_url))
}

/// `{base}/chat/completions`（OpenAI SDK 的 `baseURL` 已含 `/v1`）。
pub(crate) fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", trimmed(base_url))
}

/// `{base}/responses`。
pub(crate) fn responses_url(base_url: &str) -> String {
    format!("{}/responses", trimmed(base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_sdk_rules() {
        assert_eq!(
            anthropic_messages_url("http://127.0.0.1:9/"),
            "http://127.0.0.1:9/v1/messages?beta=true"
        );
        assert_eq!(
            anthropic_messages_url(" https://api.z.ai/api/anthropic "),
            "https://api.z.ai/api/anthropic/v1/messages?beta=true"
        );
        assert_eq!(
            anthropic_messages_url("https://gw.example.com/V1/"),
            "https://gw.example.com/V1/messages?beta=true"
        );
        assert_eq!(
            chat_completions_url("https://api.example.com/v4/"),
            "https://api.example.com/v4/chat/completions"
        );
        assert_eq!(
            responses_url("https://api.example.com/v1"),
            "https://api.example.com/v1/responses"
        );
    }
}
