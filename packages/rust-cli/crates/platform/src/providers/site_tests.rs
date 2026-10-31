//! 修复线 F23：URL 规范化后命中 providerSiteRules；各官方站点得到的能力（platform.md §3.2 第 7 条、§3.3）。

use rust_cli_base::{hosts, models};
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
        hosts::ZAI_ANTHROPIC,
        "https://API.Z.AI/api/anthropic",
        &format!("{}:443/api/anthropic//", hosts::ZAI_API),
    ] {
        let config = effective("anthropic-messages", base_url, models::GLM_5_1);
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
        &format!("{}/API/anthropic", hosts::ZAI_API),
        models::GLM_5_1,
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
            models::DEEPSEEK_V4_PRO,
        ),
        None,
    );
    let snapshot = snapshot(&resolved, &Default::default());
    let template = snapshot
        .find(&sel("personal:site", models::DEEPSEEK_V4_PRO))
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
    let official = site(anthropic, hosts::ANTHROPIC_API_V1, models::OPUS_5_5);
    assert!(official.supports_native_web_search);
    assert!(official.supports_mid_conversation_system);
    assert!(official.supports_image && official.supports_pdf);
    assert!(official.supports_json_schema_output);
    assert_eq!(official.context_window, 1_000_000);
    // 同一个模型在别的网关上：没有 WebSearch，其余照模型规则。
    let gateway = site(anthropic, "https://gw.example.com", models::OPUS_5_5);
    assert!(!gateway.supports_native_web_search);
    assert!(gateway.supports_mid_conversation_system);
    for base_url in [
        hosts::ZAI_ANTHROPIC,
        hosts::BIGMODEL_ANTHROPIC,
        hosts::ZCODE_PLAN_ANTHROPIC,
        hosts::ZCODE_HIGHSPEED_ANTHROPIC,
    ] {
        let glm = site(anthropic, base_url, models::GLM_5_1);
        assert!(glm.supports_native_web_search, "{base_url}");
        assert!(glm.supports_mid_conversation_system, "{base_url}");
        assert!(glm.supports_image && glm.supports_video, "{base_url}");
    }
    let off_peak = site(anthropic, hosts::ZCODE_OFF_PEAK_ANTHROPIC, models::GLM_5_1);
    assert!(!off_peak.supports_native_web_search);
    assert!(off_peak.supports_mid_conversation_system);
    let deepseek = site(
        anthropic,
        hosts::DEEPSEEK_ANTHROPIC,
        models::DEEPSEEK_V4_PRO,
    );
    assert!(deepseek.supports_native_web_search && deepseek.supports_mid_conversation_system);
    let kimi = site(
        anthropic,
        "https://api.moonshot.cn/anthropic",
        models::KIMI_K3,
    );
    assert!(kimi.requires_mfjs_tool_schema);
    // OpenAI 协议的同一站点不套 Anthropic 端点的规则（用没有卡片形状的模型：GLM 的对话中间 system 另由形状给，F121）。
    let chat = site(
        WireProtocol::OpenAiChatCompletions,
        hosts::ZAI_OPENAI,
        models::DEEPSEEK_V4_PRO,
    );
    assert!(!chat.supports_native_web_search && !chat.supports_mid_conversation_system);
    // 解析不了的 URL：站点规则都不命中。
    let broken = site(anthropic, "not a url", models::OPUS_5_5);
    assert!(!broken.supports_native_web_search);
}

/// 修复线 F61：显式覆盖的压缩上限与能力同一份规则——窗口是 `contextWindow`，输出上限是
/// `optionSpecs.maxOutputTokens.max`；与 Provider 配置里同一个模型的模板相同。
#[test]
fn site_rules_give_the_compaction_limits() {
    let rules = SiteRules::new(&builtin());
    let anthropic = WireProtocol::AnthropicMessages;
    let limits = |model: &str| rules.limits(anthropic, "http://127.0.0.1:9/v1", model);
    let glm = limits(models::GLM_5_3).unwrap();
    assert_eq!(
        (glm.context_window, glm.max_output_tokens),
        (1_000_000, Some(128_000))
    );
    let flash = limits(models::GLM_4_FLASH).unwrap();
    assert_eq!(
        (flash.context_window, flash.max_output_tokens),
        (131_072, Some(16_384))
    );
    // 第一条规则（`.*`）对任何模型都给窗口与输出上限。
    let card = limits(models::OPUS_5_5).unwrap();
    assert_eq!(
        (card.context_window, card.max_output_tokens),
        (1_000_000, Some(128_000))
    );
    // 与目录里同一个模型的值相同。
    let resolved = resolve(
        &builtin(),
        &personal(
            "anthropic-messages",
            "http://127.0.0.1:9/v1",
            models::GLM_5_3,
        ),
        None,
    );
    let snapshot = snapshot(&resolved, &Default::default());
    assert_eq!(
        snapshot.limits(&sel("personal:site", models::GLM_5_3)),
        Some(glm)
    );
    // 规则为空（内置配置读不了）：答不出，压缩按卡片与默认窗口。
    assert_eq!(
        SiteRules::default().limits(anthropic, "http://127.0.0.1:9/v1", models::GLM_5_3),
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

/// 工作线 F90（tools-more.md §5 的“是否提供”）：GLM-5.3-Flash 在账号 provider 上的生效配置与 Node 的规则解析
/// （`packages/provider` 的 `ModelConfigRules.resolve`，同一份内置配置）相同——bigmodel / z.ai 的团队版与个人版、
/// start plan 有原生 WebSearch，off-peak 没有；输出上限 128000，推理档位 low / high / max。期望值取自 Node 的解析。
#[test]
fn glm_5_3_flash_follows_the_node_rules() {
    let resolved = resolve(&builtin(), &json!({}), None);
    let effective = |provider_id: &str| {
        let provider = resolved
            .providers
            .iter()
            .find(|provider| provider.provider_id == provider_id)
            .unwrap_or_else(|| panic!("{provider_id}"));
        provider
            .models
            .iter()
            .find(|model| model.model_id == models::GLM_5_3_FLASH_ACCOUNT)
            .unwrap_or_else(|| panic!("{provider_id}"))
            .effective_config
            .clone()
    };
    for (provider_id, web_search) in [
        ("account:bigmodel-team-coding-plan", true),
        ("account:bigmodel-individual-coding-plan", true),
        ("account:zai-team-coding-plan", true),
        ("account:zai-individual-coding-plan", true),
        ("account:zai-start-plan", true),
        ("account:zai-offpeak-idle-plan", false),
    ] {
        let config = effective(provider_id);
        let capabilities =
            crate::providers::catalog::capabilities(models::GLM_5_3_FLASH_ACCOUNT, &config);
        assert_eq!(
            capabilities.supports_native_web_search, web_search,
            "{provider_id}"
        );
        let specs = &config["optionSpecs"];
        assert_eq!(specs["maxOutputTokens"]["max"], 128_000, "{provider_id}");
        // 工作线 F93：能力里的输出上限就是这个字段（WebSearch 旁路请求的 `min(4096, 输出上限)`）。
        assert_eq!(
            capabilities.max_output_tokens,
            Some(128_000),
            "{provider_id}"
        );
        assert_eq!(
            specs["reasoningLevel"]["values"],
            json!(["low", "high", "max"]),
            "{provider_id}"
        );
    }
}

/// 工作线 F93（docs/specs/rust-cli/model.md §11）：显式覆盖的内置规则给出的能力带配置的输出上限，与压缩的上限同值；
/// 配置的上限低于 4096 的模型（glm-4v-flash）照样给出。
#[test]
fn the_capabilities_carry_the_configured_output_limit() {
    let rules = SiteRules::new(&builtin());
    let anthropic = WireProtocol::AnthropicMessages;
    for (base_url, model) in [
        (hosts::ANTHROPIC_API_V1, models::OPUS_5_5),
        (hosts::BIGMODEL_ANTHROPIC, models::GLM_4V_FLASH),
        (hosts::BIGMODEL_ANTHROPIC, models::GLM_5_3),
    ] {
        let capabilities = rules.capabilities(anthropic, base_url, model);
        let limits = rules.limits(anthropic, base_url, model).unwrap();
        assert!(capabilities.max_output_tokens.is_some(), "{model}");
        assert_eq!(
            capabilities.max_output_tokens, limits.max_output_tokens,
            "{model}"
        );
    }
    assert_eq!(
        rules
            .capabilities(anthropic, hosts::BIGMODEL_ANTHROPIC, models::GLM_4V_FLASH)
            .max_output_tokens,
        Some(1024)
    );
}

/// 工作线 F121（model.md §11.1）：GLM 按 opus 的卡片形状，对话中间的 system 是形状的一部分——内置配置给假的站点上 GLM
/// 也为真；别的能力照配置（这里 WebSearch 照配置是假）；不是 GLM 的模型照配置。
#[test]
fn glm_always_takes_mid_conversation_system() {
    let anthropic = WireProtocol::AnthropicMessages;
    let glm = site(anthropic, "https://gw.example.com", models::GLM_5_3);
    assert!(glm.supports_mid_conversation_system);
    assert!(!glm.supports_native_web_search);
    let plain = site(anthropic, "https://gw.example.com", models::DEEPSEEK_V4_PRO);
    assert!(!plain.supports_mid_conversation_system);
}
