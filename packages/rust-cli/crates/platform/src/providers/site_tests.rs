//! 修复线 F23：URL 规范化后命中 providerSiteRules；各官方站点得到的能力（platform.md §3.2 第 7 条、§3.3）。

use rust_cli_kernel_api::provider::{ModelCapabilities, WireProtocol};
use serde_json::{Value, json};

use super::SiteRules;
use crate::providers::BUNDLED_BUILTIN;
use crate::providers::catalog::snapshot;
use crate::providers::resolve::resolve;

fn builtin() -> Value {
    serde_json::from_str::<Value>(BUNDLED_BUILTIN).unwrap()["config"].clone()
}

/// 个人配置里一个 API key 的 provider（`baseUrl` 照写），只有一个模型。
fn personal(api_type: &str, base_url: &str, model: &str) -> Value {
    json!({
        "providerConfigRules": {"providerRules": [{
            "providerId": "personal:site",
            "providerName": "Site",
            "enabled": true,
            "config": {
                "group": "standard-personal",
                "access": {"type": "api-key", "apiKey": "sk-test"},
                "api": {"type": api_type, "baseUrl": base_url},
                "personalModelIds": [model]
            }
        }]}
    })
}

fn effective(api_type: &str, base_url: &str, model: &str) -> Value {
    let resolved = resolve(&builtin(), &personal(api_type, base_url, model), None);
    let provider = resolved
        .providers
        .iter()
        .find(|provider| provider.provider_id == "personal:site")
        .unwrap();
    provider.models[0].effective_config.clone()
}

/// 修 bug：host 大写、带 `:443`、末尾多个 `/` 时，以前命中不了 z.ai 的站点规则（只去一个 `/`），WebSearch 与
/// 对话中间的 system 都丢了。Node 先规范化 URL。
#[test]
fn site_rules_match_after_the_url_is_normalized() {
    for base_url in [
        "https://api.z.ai/api/anthropic",
        "https://API.Z.AI/api/anthropic",
        "https://api.z.ai:443/api/anthropic//",
    ] {
        let config = effective("anthropic-messages", base_url, "glm-5.1");
        let properties = &config["properties"];
        assert_eq!(properties["supportsNativeWebSearch"], true, "{base_url}");
        assert_eq!(
            properties["supportsMidConversationSystem"], true,
            "{base_url}"
        );
        assert_eq!(
            properties["inputFormat"]["supportsImage"], true,
            "{base_url}"
        );
    }
    // path 的大小写不改：`/API/anthropic` 不是 z.ai 的站点。
    let other = effective(
        "anthropic-messages",
        "https://api.z.ai/API/anthropic",
        "glm-5.1",
    );
    assert_eq!(other["properties"]["supportsNativeWebSearch"], false);
}

/// 目录项与端点的能力从同一份生效配置读出。
#[test]
fn the_catalog_entry_carries_the_capabilities() {
    let resolved = resolve(
        &builtin(),
        &personal(
            "anthropic-messages",
            "https://API.deepseek.com/anthropic/",
            "deepseek-v4-pro",
        ),
        None,
    );
    let snapshot = snapshot(&resolved, &Default::default());
    let template = snapshot
        .find(&sel("personal:site", "deepseek-v4-pro"))
        .unwrap();
    let entry = template.catalog_entry();
    assert_eq!(entry.capabilities, template.capabilities);
    assert_eq!(entry.context_window, template.capabilities.context_window);
    assert!(entry.capabilities.supports_native_web_search);
    assert!(entry.capabilities.supports_mid_conversation_system);
}

fn site(protocol: WireProtocol, base_url: &str, model: &str) -> ModelCapabilities {
    SiteRules::new(&builtin()).capabilities(protocol, base_url, model)
}

/// 各官方站点在内置规则下的能力（显式覆盖与目录用同一套规则）。
#[test]
fn official_sites_get_their_capabilities() {
    let anthropic = WireProtocol::AnthropicMessages;
    let opus = site(anthropic, "https://api.anthropic.com/v1", "claude-opus-5-5");
    assert!(opus.supports_native_web_search);
    assert!(opus.supports_mid_conversation_system);
    assert!(opus.supports_image && opus.supports_pdf);
    assert!(opus.supports_json_schema_output);
    assert_eq!(opus.context_window, 1_000_000);
    // 同一个模型在别的网关上：没有 WebSearch，其余照模型规则。
    let gateway = site(anthropic, "https://gw.example.com", "claude-opus-5-5");
    assert!(!gateway.supports_native_web_search);
    assert!(gateway.supports_mid_conversation_system);
    for base_url in [
        "https://api.z.ai/api/anthropic",
        "https://open.bigmodel.cn/api/anthropic",
        "https://zcode.z.ai/api/v1/zcode-plan/anthropic",
        "https://zcode.z.ai/api/v1/highspeed/anthropic",
    ] {
        let glm = site(anthropic, base_url, "glm-5.1");
        assert!(glm.supports_native_web_search, "{base_url}");
        assert!(glm.supports_mid_conversation_system, "{base_url}");
        assert!(glm.supports_image && glm.supports_video, "{base_url}");
    }
    let off_peak = site(
        anthropic,
        "https://zcode.z.ai/api/v1/off-peak/anthropic",
        "glm-5.1",
    );
    assert!(!off_peak.supports_native_web_search);
    assert!(off_peak.supports_mid_conversation_system);
    let deepseek = site(
        anthropic,
        "https://api.deepseek.com/anthropic",
        "deepseek-v4-pro",
    );
    assert!(deepseek.supports_native_web_search && deepseek.supports_mid_conversation_system);
    let kimi = site(anthropic, "https://api.moonshot.cn/anthropic", "kimi-k3");
    assert!(kimi.requires_mfjs_tool_schema);
    // OpenAI 协议的同一站点不套 Anthropic 端点的规则。
    let chat = site(
        WireProtocol::OpenAiChatCompletions,
        "https://api.z.ai/api/paas/v4",
        "glm-5.1",
    );
    assert!(!chat.supports_native_web_search && !chat.supports_mid_conversation_system);
    // 解析不了的 URL：站点规则都不命中。
    let broken = site(anthropic, "not a url", "claude-opus-5-5");
    assert!(!broken.supports_native_web_search);
}

/// 修复线 F61：显式覆盖的压缩上限与能力同一份规则——窗口是 `contextWindow`，输出上限是
/// `optionSpecs.maxOutputTokens.max`；与 Provider 配置里同一个模型的模板相同。
#[test]
fn site_rules_give_the_compaction_limits() {
    let rules = SiteRules::new(&builtin());
    let anthropic = WireProtocol::AnthropicMessages;
    let limits = |model: &str| rules.limits(anthropic, "http://127.0.0.1:9/v1", model);
    let glm = limits("glm-5.3").unwrap();
    assert_eq!(
        (glm.context_window, glm.max_output_tokens),
        (1_000_000, Some(128_000))
    );
    let flash = limits("glm-4-flash-250414").unwrap();
    assert_eq!(
        (flash.context_window, flash.max_output_tokens),
        (131_072, Some(16_384))
    );
    // 第一条规则（`.*`）对任何模型都给窗口与输出上限。
    let opus = limits("claude-opus-5-5").unwrap();
    assert_eq!(
        (opus.context_window, opus.max_output_tokens),
        (1_000_000, Some(128_000))
    );
    // 与目录里同一个模型的值相同。
    let resolved = resolve(
        &builtin(),
        &personal("anthropic-messages", "http://127.0.0.1:9/v1", "glm-5.3"),
        None,
    );
    let snapshot = snapshot(&resolved, &Default::default());
    assert_eq!(snapshot.limits(&sel("personal:site", "glm-5.3")), Some(glm));
    // 规则为空（内置配置读不了）：答不出，压缩按卡片与默认窗口。
    assert_eq!(
        SiteRules::default().limits(anthropic, "http://127.0.0.1:9/v1", "glm-5.3"),
        None
    );
}

/// 不带团队项目的选择（按 provider 与模型找模板）。
fn sel(provider: &str, model: &str) -> rust_cli_kernel_api::model::ModelSelection {
    rust_cli_kernel_api::model::ModelSelection {
        provider: provider.to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    }
}
