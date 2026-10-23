// 复制自 apps/zcode-host/crates/providers/src/providers/resolve.rs（1f0bfd1b71）的 `resolve`。改动：只保留
// CLI 执行与目录需要的字段（provider / 模型的有效配置与可执行性），去掉 UI 视图用的分层原文与 issue 详情；
// 账号层换成本工程的 [`AccountLayer`]。
//! 三层解析（docs/specs/rust-cli/platform.md §3.2）：内置 → 账号 overlay（只作用于内置 provider，去掉
//! group）→ 个人。账号层缺失时 account provider 一律按未授权处理（fail-closed）。

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::rules::{
    Rule, array, clear_manual, ids, model_complete, overlay, owned_order, provider_issues, rules,
    text,
};

/// 第三层账号 overlay：providers 只含 `access` / `builtinModelIds`；states 是各账号 provider 的连接状态
/// （`current: false` 表示不是当前账号的连接）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountLayer {
    pub providers: Map<String, Value>,
    pub states: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelEntry {
    pub model_id: String,
    pub effective_config: Value,
    pub executable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderEntry {
    pub provider_id: String,
    pub provider_name: Option<String>,
    pub effective_config: Value,
    pub executable: bool,
    pub hidden: bool,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Resolved {
    pub providers: Vec<ProviderEntry>,
    pub default_selection: Value,
}

fn template_of<'a>(templates: &'a [Value], id: Option<&str>) -> Option<&'a Value> {
    let id = id?;
    templates
        .iter()
        .find(|template| template["templateId"] == id)
}

/// 套用模板：模板配置在下，规则配置在上。返回模板是否缺失。
fn apply_template(next: &mut Value, templates: &[Value]) -> bool {
    let template = template_of(templates, next["templateId"].as_str());
    let missing = next["templateId"].is_string() && template.is_none();
    if let Some(template) = template {
        let mut config = template["config"].clone();
        overlay(&mut config, &next["config"]);
        next["config"] = config;
    }
    missing
}

pub(crate) fn resolve(
    builtin: &Value,
    personal: &Value,
    account: Option<&AccountLayer>,
) -> Resolved {
    let templates: Vec<Value> = array(&builtin["providerConfigRules"]["templateRules"]).to_vec();
    let mut builtin_ids = Vec::new();
    let mut personal_ids: Vec<String> = Vec::new();
    // id → (合并后的 rule, 缺模板)
    let mut merged: HashMap<String, (Value, bool)> = HashMap::new();

    for rule in array(&builtin["providerConfigRules"]["providerRules"]) {
        let id = text(rule, "providerId");
        if id.is_empty() || id == "builtin:zapi" || merged.contains_key(id) {
            continue;
        }
        builtin_ids.push(id.to_owned());
        let mut next = rule.clone();
        if next["config"]["access"]["type"] == "zhipu-account" {
            match account.and_then(|layer| layer.providers.get(id)) {
                Some(layer_config) => {
                    let mut layer_config = layer_config.clone();
                    if let Some(object) = layer_config.as_object_mut() {
                        object.remove("group");
                    }
                    overlay(&mut next["config"], &layer_config);
                }
                // 账号层未发布或没有该 provider 的结果：不授权。
                None => next["config"]["access"]["entitled"] = Value::Bool(false),
            }
        }
        let missing = apply_template(&mut next, &templates);
        merged.insert(id.to_owned(), (next, missing));
    }
    for rule in array(&personal["providerConfigRules"]["providerRules"]) {
        let id = text(rule, "providerId");
        if id.is_empty() || personal_ids.iter().any(|known| known == id) {
            continue;
        }
        personal_ids.push(id.to_owned());
        let mut next = rule.clone();
        match merged.get_mut(id) {
            Some((current, _)) => {
                if let Some(config) = next["config"].as_object_mut() {
                    config.remove("group");
                }
                overlay(current, &next);
            }
            None => {
                let missing = apply_template(&mut next, &templates);
                merged.insert(id.to_owned(), (next, missing));
            }
        }
    }

    let mut model_rules = rules(&builtin["modelConfigRules"], false);
    model_rules.extend(rules(&personal["modelConfigRules"], true));

    let mut ordered = owned_order(&builtin_ids, &personal_ids, &personal["providerOrder"]);
    ordered.sort_by_key(|id| {
        let group = merged
            .get(id)
            .and_then(|(rule, _)| rule["config"]["group"].as_str());
        if matches!(group, Some("zai-family" | "bigmodel-family")) {
            0
        } else {
            1
        }
    });

    let mut providers = Vec::new();
    for id in ordered {
        let Some((rule, missing_template)) = merged.get(&id) else {
            continue;
        };
        let config = &rule["config"];
        let account_access = config["access"]["type"] == "zhipu-account";
        let enabled = account_access || rule["enabled"] != false;
        let issues = provider_issues(config, *missing_template);
        let entitled = !account_access || config["access"]["entitled"] == true;
        // 非当前账号连接保留设置展示，但不向 Registry 发布模型（off-peak 不定义 current）。
        let account_current = account
            .and_then(|layer| layer.states.get(&id))
            .is_none_or(|state| state["current"] != false);
        let executable = enabled && entitled && account_current && issues.is_empty();
        let builtin_models = ids(&config["builtinModelIds"]);
        let personal_models: Vec<String> = ids(&config["personalModelIds"])
            .into_iter()
            .filter(|model| !builtin_models.contains(model))
            .collect();
        let models = owned_order(&builtin_models, &personal_models, &config["modelOrder"])
            .into_iter()
            .map(|model_id| {
                let effective = model_config(&model_rules, &id, rule, &model_id);
                let model_executable =
                    executable && effective["enabled"] == true && model_complete(&effective);
                ModelEntry {
                    model_id,
                    effective_config: effective,
                    executable: model_executable,
                }
            })
            .collect();
        providers.push(ProviderEntry {
            provider_name: rule["providerName"].as_str().map(str::to_owned),
            effective_config: config.clone(),
            hidden: config["visibility"] == "hidden",
            provider_id: id,
            executable,
            models,
        });
    }
    Resolved {
        providers,
        default_selection: personal["defaultModelSelection"].clone(),
    }
}

/// 一个模型的有效配置：按分组顺序套用匹配的规则（手动规则先清掉用户可编辑的叶子）。
fn model_config(model_rules: &[Rule], provider_id: &str, rule: &Value, model_id: &str) -> Value {
    let config = &rule["config"];
    let base_url = config["api"]["baseUrl"].as_str().unwrap_or("");
    let api_type = text(&config["api"], "type");
    overlay_rules(model_rules, provider_id, rule, model_id, api_type, base_url)
}

/// 没有 provider 时（显式覆盖的端点，model.md §6）按内置配置的通用规则算能力：只有 modelRules、modelApiRules、
/// providerSiteRules 适用（模板与 provider 精确规则都要 provider）。
pub(crate) fn site_config(
    model_rules: &[Rule],
    api_type: &str,
    base_url: &str,
    model_id: &str,
) -> Value {
    overlay_rules(model_rules, "", &Value::Null, model_id, api_type, base_url)
}

fn overlay_rules(
    model_rules: &[Rule],
    provider_id: &str,
    rule: &Value,
    model_id: &str,
    api_type: &str,
    base_url: &str,
) -> Value {
    // 修 bug（修复线 F23）：以前只去掉末尾的 `/` 就拿来匹配，大写的 host、`:443` 命中不了站点规则（z.ai 的
    // WebSearch 与对话中间的 system 因此丢失）。按 Node 先规范化；解析不了时站点规则都不命中（Node 同样跳过）。
    let base_url = rust_cli_base::rule_url::normalize(base_url);
    let mut effective = json!({});
    for entry in model_rules {
        let value = &entry.value;
        if value.get("providerId").is_some()
            && (value["providerId"] != provider_id || value["modelId"] != model_id)
        {
            continue;
        }
        if value.get("templateId").is_some()
            && (value["templateId"] != rule["templateId"] || value["modelId"] != model_id)
        {
            continue;
        }
        let mismatch = |regex: &Option<regress::Regex>, subject: &str| {
            regex.as_ref().is_some_and(|re| re.find(subject).is_none())
        };
        let url_mismatch = entry.url.is_some()
            && base_url
                .as_deref()
                .is_none_or(|url| mismatch(&entry.url, url));
        if mismatch(&entry.model, model_id) || mismatch(&entry.api, api_type) || url_mismatch {
            continue;
        }
        if entry.manual {
            clear_manual(&mut effective);
        }
        overlay(&mut effective, &value["config"]);
    }
    effective
}
