//! 没有 provider 时的模型能力（显式覆盖的端点，docs/specs/rust-cli/model.md §6、platform.md §3.3，修复线 F23）。
//!
//! 只套内置配置的通用规则（modelRules → modelApiRules → providerSiteRules）；模板与 provider 精确规则要 provider，
//! 不适用。URL 同样先规范化（`resolve::site_config`）。

use rust_cli_kernel_api::provider::{ModelCapabilities, TokenLimits, WireProtocol};
use serde_json::Value;

use super::catalog::capabilities;
use super::resolve::site_config;
use super::rules::{Rule, rules};

/// 编译好的内置通用规则。
#[derive(Default)]
pub struct SiteRules {
    rules: Vec<Rule>,
}

impl SiteRules {
    /// `builtin` 是内置配置的 `config`（`Registry::builtin_config`）。
    pub(crate) fn new(builtin: &Value) -> Self {
        Self {
            rules: rules(&builtin["modelConfigRules"], false),
        }
    }

    /// （协议，baseUrl，模型 ID）按内置规则得到的能力。
    pub fn capabilities(
        &self,
        protocol: WireProtocol,
        base_url: &str,
        model: &str,
    ) -> ModelCapabilities {
        capabilities(
            &self.effective(protocol, base_url, model),
            protocol,
            base_url,
        )
    }

    /// 同一份规则给出的压缩上限（修复线 F61，compact.md §2）：`contextWindow` 与 `optionSpecs.maxOutputTokens.max`。
    /// 规则里没有窗口（例如内置配置读不了，规则为空）时答不出，压缩按卡片与默认窗口；没有 max 时输出上限为 None。
    pub fn limits(
        &self,
        protocol: WireProtocol,
        base_url: &str,
        model: &str,
    ) -> Option<TokenLimits> {
        let effective = self.effective(protocol, base_url, model);
        let context_window = effective["properties"]["contextWindow"]
            .as_u64()
            .filter(|window| *window > 0)?;
        Some(TokenLimits {
            context_window,
            max_output_tokens: effective["optionSpecs"]["maxOutputTokens"]["max"].as_u64(),
        })
    }

    fn effective(&self, protocol: WireProtocol, base_url: &str, model: &str) -> Value {
        let api_type = match protocol {
            WireProtocol::AnthropicMessages => "anthropic-messages",
            WireProtocol::OpenAiChatCompletions => "openai-chat-completions",
            WireProtocol::OpenAiResponses => "openai-responses",
        };
        site_config(&self.rules, api_type, base_url, model)
    }
}

#[cfg(test)]
#[path = "site_tests.rs"]
mod tests;
