//! 解析结果与 Node 一致（platform.md §7）：同一份内置配置、个人配置夹具与账号 overlay，Node 的 Provider
//! registry 给出的可执行模型（`tests/fixtures/node-registry.json`，由 `scripts/rust-cli/provider-registry-golden.mts`
//! 生成）与这里的解析结果逐项相同。

use serde_json::{Map, Value, json};

use super::*;
use crate::providers::BUNDLED_BUILTIN;
use crate::providers::resolve::{AccountLayer, resolve};

const PERSONAL: &str = include_str!("../../tests/fixtures/personal.json");
const NODE_REGISTRY: &str = include_str!("../../tests/fixtures/node-registry.json");
const ENTITLED: &str = "account:zai-individual-coding-plan";

fn layer(builtin: &Value, entitled: &str) -> AccountLayer {
    let mut providers = Map::new();
    for rule in builtin["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
    {
        if rule["config"]["access"]["type"] == "zhipu-account" {
            let id = rule["providerId"].as_str().unwrap();
            providers.insert(
                id.to_owned(),
                json!({"access": {"type": "zhipu-account", "entitled": id == entitled}}),
            );
        }
    }
    AccountLayer {
        providers,
        states: Map::new(),
    }
}

fn fixture_snapshot() -> Snapshot {
    let builtin: Value = serde_json::from_str(BUNDLED_BUILTIN).unwrap();
    let personal: Value = serde_json::from_str(PERSONAL).unwrap();
    let account = layer(&builtin, ENTITLED);
    snapshot(
        &resolve(&builtin["config"], &personal["config"], Some(&account)),
        &Default::default(),
    )
}

#[test]
fn executable_models_match_the_node_registry() {
    let ours: Vec<Value> = fixture_snapshot()
        .templates
        .iter()
        .map(|template| {
            json!({
                "provider": template.provider,
                "model": template.model,
                "apiType": template.protocol,
                "baseUrl": template.base_url,
                "hidden": template.hidden,
                "reasoningLevels": template.levels,
                "contextWindow": template.capabilities.context_window,
                "maxOutputTokens": template.max_output_tokens,
            })
        })
        .collect();
    let node: Vec<Value> = serde_json::from_str(NODE_REGISTRY).unwrap();
    assert_eq!(ours, node);
}

#[test]
fn the_bundled_builtin_is_the_repository_copy() {
    // 仓库的内置配置更新后，随包的副本要一起更新（platform.md §3.1）。仓库的那份不在 `packages/rust-cli` 下，
    // Windows 上可能检出成 CRLF；随包的副本固定 LF（修复线 F56），所以按 LF 比较。
    let repository = include_str!("../../../../../../config/provider/zcode-builtin.json");
    assert_eq!(BUNDLED_BUILTIN, repository.replace("\r\n", "\n"));
}

#[test]
fn without_an_account_layer_account_providers_are_closed() {
    let builtin: Value = serde_json::from_str(BUNDLED_BUILTIN).unwrap();
    let resolved = resolve(&builtin["config"], &json!({}), None);
    let snapshot = snapshot(&resolved, &Default::default());
    assert!(snapshot.templates.is_empty(), "{:?}", snapshot.templates);
    assert_eq!(snapshot.default, None);
}

#[test]
fn the_default_selection_follows_the_personal_config() {
    let snapshot = fixture_snapshot();
    assert_eq!(
        snapshot.default,
        Some(ModelSelection {
            provider: "personal:gateway".to_owned(),
            model: "custom-chat".to_owned(),
            reasoning: Some("low".to_owned()),
            connection: None,
        })
    );
}

/// Node `resolveInitialModelSelection`（workspace-trust.md §3.1）：配置的缺省“可选”时原样用它，否则目录第一项、
/// 最后一个档位。目录第一项是 `account:zai-individual-coding-plan/GLM-5.3`（档位到 max）。
#[test]
fn an_unusable_default_falls_back_to_the_first_model_like_node() {
    let snapshot = fixture_snapshot();
    let first = Some(ModelSelection {
        provider: ENTITLED.to_owned(),
        model: "GLM-5.3".to_owned(),
        reasoning: Some("max".to_owned()),
        connection: None,
    });
    let resolve = |configured: Value| default_selection(&snapshot.templates, &configured);
    // 没有档位（登录写的形状、`/effort auto`）：Node 判为不可选。
    assert_eq!(
        resolve(json!({"providerId": "personal:gateway", "modelId": "custom-chat"})),
        first
    );
    // 档位不在模型的档位里。
    assert_eq!(
        resolve(
            json!({"providerId": "personal:gateway", "modelId": "custom-chat", "options": {"reasoningLevel": "max"}})
        ),
        first
    );
    // 模型不在目录里、没有缺省。
    assert_eq!(
        resolve(
            json!({"providerId": "personal:gateway", "modelId": "gone", "options": {"reasoningLevel": "low"}})
        ),
        first
    );
    assert_eq!(resolve(Value::Null), first);
    // 可选：原样用它，包括档位。
    assert_eq!(
        resolve(
            json!({"providerId": "personal:gateway", "modelId": "glm-5", "options": {"reasoningLevel": "enabled"}})
        ),
        Some(ModelSelection {
            provider: "personal:gateway".to_owned(),
            model: "glm-5".to_owned(),
            reasoning: Some("enabled".to_owned()),
            connection: None,
        })
    );
    // 隐藏的 provider 不可选，也不当目录第一项。
    let mut templates = snapshot.templates.clone();
    for template in &mut templates {
        if template.provider == ENTITLED || template.provider == "personal:gateway" {
            template.hidden = true;
        }
    }
    let fallback = default_selection(
        &templates,
        &json!({"providerId": "personal:gateway", "modelId": "glm-5", "options": {"reasoningLevel": "enabled"}}),
    )
    .unwrap();
    assert_eq!(fallback.provider, "personal:responses");
    assert_eq!(
        fallback.reasoning.as_deref(),
        templates
            .iter()
            .find(|template| template.provider == "personal:responses")
            .and_then(|template| template.levels.last().map(String::as_str))
    );
}

#[test]
fn option_maps_become_request_patches() {
    let snapshot = fixture_snapshot();
    let glm = snapshot.find(&sel(ENTITLED, "GLM-5.3")).unwrap();
    assert_eq!(glm.protocol, WireProtocol::AnthropicMessages);
    assert_eq!(
        glm.access,
        Access::Account {
            account_type: "zai".to_owned(),
            mode: "individual-coding-plan".to_owned(),
        }
    );
    assert_eq!(
        glm.patches(Some("high"), None).unwrap(),
        vec![
            json!({"thinking": {"type": "enabled"}, "output_config": {"effort": "high"}}),
            json!({"max_tokens": 128000}),
        ]
    );
    let gateway = snapshot.find(&sel("personal:gateway", "glm-5")).unwrap();
    assert_eq!(
        gateway.access,
        Access::ApiKey(RedactedString::new("sk-personal-fixture"))
    );
    assert_eq!(
        gateway.headers,
        vec![("X-Team".to_owned(), RedactedString::new("dev"))]
    );
    // 没指定档位时用最后一个（enabled）。
    let patches = gateway.patches(None, None).unwrap();
    assert_eq!(patches[0]["reasoning_effort"], "high");
    assert_eq!(patches[1], json!({"max_completion_tokens": 64000}));
    assert!(matches!(
        gateway.patches(Some("max"), None),
        Err(ModelError::InvalidRequest { .. })
    ));
    // 目录不含隐藏 provider；可执行的模型都能按选择找到。
    assert_eq!(snapshot.catalog().len(), snapshot.templates.len());
    assert!(
        snapshot
            .find(&sel("personal:nokey", "claude-opus-5"))
            .is_none()
    );
}

/// `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：`maxOutputTokens.map` 用它与配置的 max 的较小值；协议
/// 自己的字段名不变（Anthropic `max_tokens`、Chat `max_completion_tokens`）。
#[test]
fn the_output_limit_caps_the_max_output_tokens_patch() {
    let snapshot = fixture_snapshot();
    let glm = snapshot.find(&sel(ENTITLED, "GLM-5.3")).unwrap();
    assert_eq!(
        glm.patches(Some("high"), Some(4096)).unwrap()[1],
        json!({"max_tokens": 4096})
    );
    assert_eq!(
        glm.patches(Some("high"), Some(500_000)).unwrap()[1],
        json!({"max_tokens": 128000})
    );
    let gateway = snapshot.find(&sel("personal:gateway", "glm-5")).unwrap();
    assert_eq!(
        gateway.patches(None, Some(4096)).unwrap()[1],
        json!({"max_completion_tokens": 4096})
    );
}

/// 显式覆盖没给模型时用的 `defaultModelSelection.modelId`（model.md §6，修复线 F42）：原样取，不看目录与档位。
#[test]
fn the_configured_model_is_the_raw_default_model_selection() {
    assert_eq!(
        configured_model(&json!({"providerId": "zai", "modelId": " GLM-5.3 "})),
        Some("GLM-5.3".to_owned())
    );
    assert_eq!(
        configured_model(&json!({"providerId": "old", "modelId": "m"})),
        Some("m".to_owned())
    );
    assert_eq!(configured_model(&json!({"modelId": ""})), None);
    assert_eq!(configured_model(&json!({"modelId": 3})), None);
    assert_eq!(configured_model(&serde_json::Value::Null), None);
    // 夹具的个人配置写了 `defaultModelSelection`：快照里带着它的模型 ID。
    let personal: Value = serde_json::from_str(PERSONAL).unwrap();
    assert_eq!(
        fixture_snapshot().configured_model,
        configured_model(&personal["config"]["defaultModelSelection"])
    );
    assert!(fixture_snapshot().configured_model.is_some());
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
