//! 工作线 F52（docs/specs/rust-cli/tools-more.md §3.2，用户决定 2026-10-22）：ToolSearch 只给 z.ai / bigmodel.cn
//! 域名下的 Anthropic 端点开，其他一律不开。目录项与显式覆盖的内置规则按端点地址判定，不看模型。

use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::WireProtocol;
use serde_json::{Value, json};

use super::endpoint;
use crate::providers::BUNDLED_BUILTIN;
use crate::providers::catalog::snapshot;
use crate::providers::resolve::resolve;
use crate::providers::site::SiteRules;

fn builtin() -> Value {
    serde_json::from_str::<Value>(BUNDLED_BUILTIN).unwrap()["config"].clone()
}

const ANTHROPIC: WireProtocol = WireProtocol::AnthropicMessages;

/// 内置配置与账号 provider 里 z.ai / bigmodel 的全部 Anthropic 端点（Coding Plan、Team Plan、Start Plan、off-peak、
/// 高速卡与两个 API key 模板）都开；写法变了（大小写、端口、末尾的 `/`）也开。
#[test]
fn zhipu_anthropic_endpoints_enable_tool_search() {
    for base_url in [
        "https://api.z.ai/api/anthropic",
        "https://open.bigmodel.cn/api/anthropic",
        "https://zcode.z.ai/api/v1/zcode-plan/anthropic",
        "https://zcode.z.ai/api/v1/off-peak/anthropic",
        "https://zcode.z.ai/api/v1/highspeed/anthropic",
        "https://API.Z.AI:443/api/anthropic//",
        "https://bigmodel.cn/api/anthropic",
        "https://z.ai/anthropic",
    ] {
        assert!(endpoint(ANTHROPIC, base_url), "{base_url}");
    }
}

/// 其他一律不开：Anthropic 官方、第三方网关（用户报错的场景：网关后面是 Bedrock）、长得像的域名、解析不了的地址，
/// 以及 OpenAI 两种协议（包括 z.ai / bigmodel 自己的 `paas/v4`）。
#[test]
fn every_other_endpoint_keeps_tool_search_off() {
    for base_url in [
        "https://api.anthropic.com/v1",
        "https://api.anthropic.com",
        "https://api-gateway.example.com",
        "http://127.0.0.1:8080",
        "https://notz.ai/api/anthropic",
        "https://z.ai.example.com/api/anthropic",
        "https://open.bigmodel.cn.example.com/api/anthropic",
        "https://example.com/api.z.ai/anthropic",
        "not a url",
        "",
    ] {
        assert!(!endpoint(ANTHROPIC, base_url), "{base_url}");
    }
    for protocol in [
        WireProtocol::OpenAiChatCompletions,
        WireProtocol::OpenAiResponses,
    ] {
        for base_url in [
            "https://api.z.ai/api/paas/v4",
            "https://open.bigmodel.cn/api/paas/v4",
        ] {
            assert!(!endpoint(protocol, base_url), "{protocol:?} {base_url}");
        }
    }
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

/// 目录：判定只看端点。用户的场景（anthropic 协议的第三方网关上的 claude-opus-5-5）不开；z.ai 上的 GLM（没有模型卡片）
/// 开；官方 Anthropic 上的 claude-opus-5-5 也不开。修 bug 前 ToolSearch 只看模型卡片，网关上的 claude-opus-5-5 带
/// `advanced-tool-use` beta，被 Bedrock 拒绝。
#[test]
fn the_catalog_decides_by_the_endpoint_not_the_model() {
    let messages = "anthropic-messages";
    assert!(!catalog_flag(
        messages,
        "https://api-gateway.example.com",
        "claude-opus-5-5"
    ));
    assert!(!catalog_flag(
        messages,
        "https://api.anthropic.com/v1",
        "claude-opus-5-5"
    ));
    assert!(catalog_flag(
        messages,
        "https://api.z.ai/api/anthropic",
        "glm-5.1"
    ));
    assert!(catalog_flag(
        messages,
        "https://open.bigmodel.cn/api/anthropic",
        "claude-opus-5-5"
    ));
    assert!(!catalog_flag(
        "openai-chat-completions",
        "https://api.z.ai/api/paas/v4",
        "glm-5.1"
    ));
}

/// 账号 provider（内置配置的 providerRules，`zhipu-account`）：端点模板的地址都在 z.ai / bigmodel.cn 下。
#[test]
fn account_provider_templates_enable_tool_search() {
    let config = builtin();
    let rules = config["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap();
    let accounts: Vec<&Value> = rules
        .iter()
        .filter(|rule| rule["config"]["access"]["type"] == "zhipu-account")
        .collect();
    assert!(accounts.len() >= 10, "{}", accounts.len());
    for rule in accounts {
        let api = &rule["config"]["api"];
        let protocol = WireProtocol::parse(api["type"].as_str().unwrap()).unwrap();
        let base_url = api["baseUrl"].as_str().unwrap();
        assert!(endpoint(protocol, base_url), "{}", rule["providerId"]);
    }
}

/// 显式覆盖的内置规则（cli 用）与目录用同一个判定。
#[test]
fn site_rules_use_the_same_decision() {
    let rules = SiteRules::new(&builtin());
    let flag = |base_url: &str, model: &str| {
        rules
            .capabilities(ANTHROPIC, base_url, model)
            .supports_tool_search
    };
    assert!(!flag("https://api.anthropic.com/v1", "claude-opus-5-5"));
    assert!(!flag("http://127.0.0.1:9/v1", "claude-opus-5-5"));
    assert!(flag("https://api.z.ai/api/anthropic/v1", "claude-opus-5-5"));
    assert!(flag("https://open.bigmodel.cn/api/anthropic/v1", "glm-5.1"));
}
