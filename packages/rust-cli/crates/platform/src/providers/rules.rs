// 复制自 apps/zcode-host/crates/providers/src/providers/resolve.rs（1f0bfd1b71）的规则部分：配置覆盖、
// 归属排序、模型规则、手动规则清理、模型完整性与 provider 校验。改动：provider 校验只给出问题代码
// （CLI 不展示 issue 详情）；只保留 CLI 执行需要的部分，UI 视图（候选、有效配置视图）没有复制。
//! Provider 配置的解析规则（docs/specs/rust-cli/platform.md §3.2）。
//!
//! 规则顺序与 Node 的 `packages/provider/src/resolver.ts` 一致：Host 列给 UI 的可选模型必须正好是
//! CLI 能执行的模型。

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::option_map::{evaluate, validate_patches};

/// 模型规则：分组顺序决定覆盖顺序。
pub(crate) struct Rule {
    pub value: Value,
    pub manual: bool,
    pub model: Option<regress::Regex>,
    pub api: Option<regress::Regex>,
    pub url: Option<regress::Regex>,
}

pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// 字符串数组，去重并保持先后。
pub(crate) fn ids(value: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    array(value)
        .iter()
        .filter_map(Value::as_str)
        .filter(|id| seen.insert(id.to_owned()))
        .map(str::to_owned)
        .collect()
}

/// 配置子树递归覆盖；headers 等字典与叶子整体替换。access 的 type 是判别字段，换类型时不继承旧密钥。
pub(crate) fn overlay(base: &mut Value, next: &Value) {
    let Some(object) = next.as_object() else {
        *base = next.clone();
        return;
    };
    if !base.is_object() {
        *base = json!({});
    }
    for (key, value) in object {
        let nested = [
            "config",
            "api",
            "access",
            "properties",
            "optionSpecs",
            "inputFormat",
            "outputFormat",
            "reasoningLevel",
            "maxOutputTokens",
        ]
        .contains(&key.as_str());
        if nested && value.is_object() {
            if key == "access" && base[key]["type"] != value["type"] {
                base[key] = value.clone();
            } else {
                overlay(&mut base[key], value);
            }
        } else {
            base[key] = value.clone();
        }
    }
}

/// `resolveOwnedOrder`：未排序的内置 → 请求的顺序 → 未排序的个人。
pub(crate) fn owned_order(
    builtin: &[String],
    personal: &[String],
    requested: &Value,
) -> Vec<String> {
    let requested: Vec<String> = array(requested)
        .iter()
        .filter_map(Value::as_str)
        .filter(|id| builtin.iter().chain(personal).any(|known| known == id))
        .map(str::to_owned)
        .collect();
    let mut result: Vec<String> = Vec::new();
    for id in builtin
        .iter()
        .filter(|id| !requested.contains(id))
        .chain(&requested)
        .chain(personal.iter().filter(|id| !requested.contains(id)))
    {
        if !result.contains(id) {
            result.push(id.clone());
        }
    }
    result
}

pub(crate) fn rules(config: &Value, personal: bool) -> Vec<Rule> {
    let groups: &[&str] = if personal {
        &["providerModelRules", "manualProviderModelRules"]
    } else {
        &[
            "modelRules",
            "modelApiRules",
            "providerSiteRules",
            "templateModelRules",
            "builtinProviderModelRules",
        ]
    };
    let mut result = Vec::new();
    for group in groups {
        for rule in array(&config[*group]) {
            // matcher 是 JS RegExp 语法，与 TS `matchesRule` 相同：`^(?:pattern)$`，模型名加 `i`。用
            // ECMAScript 语义的 `regress`（内置 GLM-4.6 规则的 `(?!v)` 需要 look-around）。
            let regex = |key: &str, insensitive: bool| {
                rule[key].as_str().and_then(|pattern| {
                    regress::Regex::with_flags(
                        &format!("^(?:{pattern})$"),
                        if insensitive { "i" } else { "" },
                    )
                    .ok()
                })
            };
            result.push(Rule {
                value: rule.clone(),
                manual: *group == "manualProviderModelRules",
                model: regex("modelMatch", true),
                api: regex("apiTypeMatch", false),
                url: regex("baseUrlMatch", false),
            });
        }
    }
    result
}

/// 手动规则只清除产品开放给用户编辑的叶子，系统参数映射继续继承。
pub(crate) fn clear_manual(value: &mut Value) {
    if let Some(properties) = value["properties"].as_object_mut() {
        for key in [
            "contextWindow",
            "supportsJsonSchemaOutput",
            "supportsNativeWebSearch",
            "supportsMidConversationSystem",
        ] {
            properties.remove(key);
        }
    }
    if let Some(input) = value["properties"]["inputFormat"].as_object_mut() {
        for key in ["supportsImage", "supportsVideo", "supportsPdf"] {
            input.remove(key);
        }
    }
    if let Some(specs) = value["optionSpecs"].as_object_mut() {
        specs.remove("reasoningLevel");
    }
    if let Some(max) = value["optionSpecs"]["maxOutputTokens"].as_object_mut() {
        max.remove("max");
    }
}

/// 完整模型配置：属性齐全、档位非空且唯一、预算为正、两个 option map 都能求值。
pub(crate) fn model_complete(config: &Value) -> bool {
    let properties = &config["properties"];
    let flags_ok = [
        "requiresMfjsToolSchema",
        "supportsToolCall",
        "supportsJsonSchemaOutput",
        "supportsNativeWebSearch",
        "supportsMidConversationSystem",
    ]
    .iter()
    .all(|key| properties[*key].is_boolean())
        && [
            "supportsText",
            "supportsImage",
            "supportsVideo",
            "supportsAudio",
            "supportsPdf",
        ]
        .iter()
        .all(|key| properties["inputFormat"][*key].is_boolean())
        && properties["outputFormat"]["supportsText"].is_boolean();
    let levels = &config["optionSpecs"]["reasoningLevel"]["values"];
    let levels_ok = levels.as_array().is_some_and(|values| {
        !values.is_empty()
            && values
                .iter()
                .all(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()))
            && values
                .iter()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>()
                .len()
                == values.len()
    });
    let window = properties["contextWindow"].as_u64().unwrap_or(0);
    let max = config["optionSpecs"]["maxOutputTokens"]["max"]
        .as_u64()
        .unwrap_or(0);
    flags_ok && levels_ok && window > 0 && max > 0 && option_maps_valid(config)
}

fn option_maps_valid(config: &Value) -> bool {
    let specs = &config["optionSpecs"];
    let (Some(reasoning_map), Some(max_map)) = (
        specs["reasoningLevel"]["map"].as_str(),
        specs["maxOutputTokens"]["map"].as_str(),
    ) else {
        return false;
    };
    let max = specs["maxOutputTokens"]["max"].clone();
    array(&specs["reasoningLevel"]["values"])
        .iter()
        .all(|level| {
            let patches = [
                evaluate(reasoning_map, "reasoningLevel", level),
                evaluate(max_map, "maxOutputTokens", &max),
            ];
            match patches {
                [Ok(a), Ok(b)] => validate_patches(&[a, b]).is_ok(),
                _ => false,
            }
        })
}

/// 完整 provider 配置校验（provider-data-schema.ts）：返回问题代码，空表示可用。
pub(crate) fn provider_issues(config: &Value, missing_template: bool) -> Vec<&'static str> {
    let mut issues = Vec::new();
    if missing_template {
        issues.push("missing-template");
    }
    if config["group"].as_str().is_none_or(str::is_empty) {
        issues.push("group");
    }
    match config["access"]["type"].as_str() {
        Some("zhipu-account") => {}
        Some("api-key" | "zhipu-coding-plan-api-key") => {
            if config["access"]["apiKey"]
                .as_str()
                .is_none_or(|key| key.trim().is_empty())
            {
                issues.push("access.apiKey");
            }
        }
        _ => issues.push("access"),
    }
    if !matches!(
        config["api"]["type"].as_str(),
        Some("anthropic-messages" | "openai-chat-completions" | "openai-responses")
    ) {
        issues.push("api.type");
    }
    let base_ok = config["api"]["baseUrl"]
        .as_str()
        .and_then(|base| url::Url::parse(base).ok())
        .is_some_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
        });
    if !base_ok {
        issues.push("api.baseUrl");
    }
    issues
}
