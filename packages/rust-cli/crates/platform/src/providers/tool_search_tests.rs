//! 工作线 F89（docs/specs/rust-cli/tools-more.md §3.2.2，用户决定 2026-10-24）：所有模型、所有端点都用本地 ToolSearch，
//! 暂时不用服务器原生的延迟加载（含 z.ai / bigmodel）。平台层的目录项与显式覆盖的内置规则共用一个能力函数，端点的
//! `supports_tool_search` 总为假；原生只能经只在 debug 构建读的测试声明选到（cli）。
//!
//! 修改：F52 到 F84 按端点算，z.ai / bigmodel.cn 域名下的 Anthropic 端点为真（原生），其余为假。

use rust_cli_base::{hosts, models};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::WireProtocol;
use serde_json::{Value, json};

use crate::providers::BUNDLED_BUILTIN;
use crate::providers::catalog::snapshot;
use crate::providers::resolve::resolve;
use crate::providers::site::SiteRules;

fn builtin() -> Value {
    serde_json::from_str::<Value>(BUNDLED_BUILTIN).unwrap()["config"].clone()
}

/// 个人配置里一个 API key 的 provider。
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

fn catalog_flag(api_type: &str, base_url: &str, model: &str) -> bool {
    let resolved = resolve(&builtin(), &personal(api_type, base_url, model), None);
    let snapshot = snapshot(&resolved, &Default::default());
    let selection = ModelSelection {
        provider: "personal:site".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    };
    let template = snapshot.find(&selection).unwrap();
    // 目录项与端点模板是同一份能力。
    assert_eq!(template.catalog_entry().capabilities, template.capabilities);
    template.capabilities.supports_tool_search
}

/// 目录：任何端点都不是原生，z.ai / bigmodel 的 Anthropic 端点也不是（以前为真）。
#[test]
fn no_catalog_endpoint_uses_native_tool_search() {
    for (api_type, base_url, model) in [
        ("anthropic-messages", hosts::ZAI_ANTHROPIC, models::GLM_5_1),
        (
            "anthropic-messages",
            hosts::BIGMODEL_ANTHROPIC,
            models::OPUS_5_5,
        ),
        (
            "anthropic-messages",
            hosts::ZCODE_PLAN_ANTHROPIC,
            models::GLM_5_1,
        ),
        (
            "anthropic-messages",
            "https://api-gateway.example.com",
            models::OPUS_5_5,
        ),
        (
            "anthropic-messages",
            hosts::ANTHROPIC_API_V1,
            models::OPUS_5_5,
        ),
        (
            "openai-chat-completions",
            hosts::ZAI_OPENAI,
            models::GLM_5_1,
        ),
    ] {
        assert!(!catalog_flag(api_type, base_url, model), "{base_url}");
    }
}

/// 账号 provider（内置配置的 providerRules，`zhipu-account`）的端点模板：地址都在 z.ai / bigmodel.cn 下，同样不是原生。
#[test]
fn account_provider_templates_use_local_tool_search() {
    let config = builtin();
    let rules = config["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap();
    let accounts: Vec<&Value> = rules
        .iter()
        .filter(|rule| rule["config"]["access"]["type"] == "zhipu-account")
        .collect();
    assert!(accounts.len() >= 10, "{}", accounts.len());
    let site = SiteRules::new(&config);
    for rule in accounts {
        let api = &rule["config"]["api"];
        let protocol = WireProtocol::parse(api["type"].as_str().unwrap()).unwrap();
        let base_url = api["baseUrl"].as_str().unwrap();
        let capabilities = site.capabilities(protocol, base_url, models::GLM_5_1);
        assert!(!capabilities.supports_tool_search, "{}", rule["providerId"]);
    }
}

/// 显式覆盖的内置规则（cli 用）与目录用同一个能力函数：z.ai 的地址上也不是原生。
#[test]
fn site_rules_use_the_same_decision() {
    let rules = SiteRules::new(&builtin());
    let flag = |base_url: &str, model: &str| {
        rules
            .capabilities(WireProtocol::AnthropicMessages, base_url, model)
            .supports_tool_search
    };
    assert!(!flag(hosts::ANTHROPIC_API_V1, models::OPUS_5_5));
    assert!(!flag("http://127.0.0.1:9/v1", models::OPUS_5_5));
    assert!(!flag(
        &format!("{}/v1", hosts::ZAI_ANTHROPIC),
        models::OPUS_5_5
    ));
    assert!(!flag(
        &format!("{}/v1", hosts::BIGMODEL_ANTHROPIC),
        models::GLM_5_1
    ));
}
