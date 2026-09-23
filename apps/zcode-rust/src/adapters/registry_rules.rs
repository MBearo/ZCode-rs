use super::registry::{ModelKey, Snapshot};
use super::registry_config::{array, clear_manual, ids, order, overlay, rules, text, valid_model};
use super::{config::ModelConfig, provider::HttpModel};
use crate::contract::{ModelIdentity, ModelPort};
use crate::domain::option_map::{evaluate, validate_patches};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
pub(super) fn validate_account(v: &Value) -> Result<()> {
    for k in ["revision", "basedOnZCodeBuiltinRevision"] {
        ensure!(
            v[k].as_str().is_some_and(|s| !s.trim().is_empty()),
            "Invalid account revision"
        );
    }
    for (id, p) in v["providers"]
        .as_object()
        .context("Invalid account providers")?
    {
        ensure!(!id.is_empty(), "Invalid account provider");
        let p = p.as_object().context("Invalid account overlay")?;
        ensure!(
            p.keys()
                .all(|k| ["access", "builtinModelIds"].contains(&k.as_str())),
            "Invalid account overlay field"
        );
        if let Some(a) = p.get("access").filter(|a| !a.is_null()) {
            ensure!(
                a["type"] == "zhipu-account"
                    && a.as_object().is_some_and(|o| o
                        .keys()
                        .all(|k| ["type", "entitled"].contains(&k.as_str()))),
                "Invalid account access"
            );
            ensure!(
                a.get("entitled").is_none_or(Value::is_boolean),
                "Invalid entitlement"
            );
        }
        if let Some(ids) = p.get("builtinModelIds").filter(|v| !v.is_null()) {
            ensure!(
                ids.as_array()
                    .is_some_and(|a| a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))),
                "Invalid account models"
            );
        }
    }
    Ok(())
}
pub(super) fn resolve(
    builtin: &Value,
    personal: &Value,
    account: &Value,
    pool: Arc<tokio::sync::OnceCell<reqwest::Client>>,
) -> Result<Snapshot> {
    let mut providers: BTreeMap<String, Value> = BTreeMap::new();
    let templates = array(&builtin["providerConfigRules"]["templateRules"]);
    let mut builtin_ids = vec![];
    let mut personal_ids = vec![];
    for (source, builtin_layer) in [(builtin, true), (personal, false)] {
        for rule in array(&source["providerConfigRules"]["providerRules"]) {
            let id = text(rule, "providerId");
            ensure!(
                !id.is_empty() && id != "builtin:zapi",
                "Invalid provider identity"
            );
            let mut next = rule.clone();
            if builtin_layer {
                builtin_ids.push(id.to_owned());
                if let Some(a) = account["providers"].get(id) {
                    overlay(&mut next["config"], a);
                }
            } else {
                personal_ids.push(id.to_owned());
                if providers.contains_key(id) {
                    next["config"]
                        .as_object_mut()
                        .context("Invalid provider config")?
                        .remove("group");
                }
            }
            if !providers.contains_key(id)
                && let Some(template_id) = next["templateId"].as_str()
            {
                let Some(template) = templates.iter().find(|t| t["templateId"] == template_id)
                else {
                    continue;
                };
                let mut config = template["config"].clone();
                overlay(&mut config, &next["config"]);
                next["config"] = config;
            }
            let current = providers.entry(id.to_owned()).or_insert_with(|| json!({}));
            overlay(current, &next);
        }
    }
    let mut rules = rules(&builtin["modelConfigRules"], false)?;
    rules.extend(super::registry_config::rules(
        &personal["modelConfigRules"],
        true,
    )?);
    let mut result = Snapshot::default();
    let mut ordered = order(builtin_ids, personal_ids, &personal["providerOrder"]);
    ordered.sort_by_key(|id| {
        if matches!(
            providers[id]["config"]["group"].as_str(),
            Some("zai-family" | "bigmodel-family")
        ) {
            0
        } else {
            1
        }
    });
    for id in ordered {
        let rule = &providers[&id];
        let config = &rule["config"];
        let account_access = config["access"]["type"] == "zhipu-account";
        if (account_access
            && (config["access"]["entitled"] != true || account["states"][&id]["current"] == false))
            || (!account_access && rule["enabled"] == false)
        {
            continue;
        }
        if !account_access
            && config["access"]["apiKey"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            continue;
        }
        let base = text(&config["api"], "baseUrl");
        let Ok(url) = reqwest::Url::parse(base) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            continue;
        }
        let normalized = url.as_str().trim_end_matches('/');
        for model in order(
            ids(&config["builtinModelIds"]),
            ids(&config["personalModelIds"]),
            &config["modelOrder"],
        ) {
            let mut mc = json!({});
            for r in &rules {
                let v = &r.value;
                if v.get("providerId").is_some() && (v["providerId"] != id || v["modelId"] != model)
                {
                    continue;
                }
                if v.get("templateId").is_some()
                    && (v["templateId"] != rule["templateId"] || v["modelId"] != model)
                {
                    continue;
                }
                if r.model.as_ref().is_some_and(|r| !r.is_match(&model))
                    || r.api
                        .as_ref()
                        .is_some_and(|r| !r.is_match(text(&config["api"], "type")))
                    || r.url.as_ref().is_some_and(|r| !r.is_match(normalized))
                {
                    continue;
                }
                if r.manual {
                    clear_manual(&mut mc);
                }
                overlay(&mut mc, &v["config"]);
            }
            if mc["enabled"] != true || !valid_model(&mc) {
                continue;
            }
            let levels = ids(&mc["optionSpecs"]["reasoningLevel"]["values"]);
            if levels.is_empty() {
                continue;
            }
            let prepared = (|| -> Result<Vec<(ModelKey, Arc<dyn ModelPort>)>> {
                let window = mc["properties"]["contextWindow"]
                    .as_u64()
                    .context("Invalid context window")?;
                let max = mc["optionSpecs"]["maxOutputTokens"]["max"]
                    .as_u64()
                    .context("Invalid max output")?;
                ensure!(max > 0 && window > 0, "Invalid model budget");
                let mut prepared = vec![];
                for level in &levels {
                    let mut c: ModelConfig = serde_json::from_value(
                        json!({"providerId":id,"modelId":model,"reasoningLevel":level,"apiType":config["api"]["type"],"baseUrl":base,"contextWindow":window,"maxOutputTokens":max}),
                    )?;
                    c.format_properties = Some(
                        json!({"inputFormat":mc["properties"]["inputFormat"],"outputFormat":mc["properties"]["outputFormat"]}),
                    );
                    for (name, input) in [
                        ("reasoningLevel", json!(level)),
                        ("maxOutputTokens", json!(max)),
                    ] {
                        c.option_patches.push(evaluate(
                            mc["optionSpecs"][name]["map"]
                                .as_str()
                                .context("Missing option map")?,
                            name,
                            &input,
                        )?);
                    }
                    validate_patches(&c.option_patches)?;
                    c.max_output_map = mc["optionSpecs"]["maxOutputTokens"]["map"]
                        .as_str()
                        .map(str::to_owned);
                    if let Some(headers) = config["api"]["headers"].as_object() {
                        for (k, v) in headers {
                            c.headers.insert(
                                k.clone(),
                                v.as_str().context("Invalid provider header")?.into(),
                            );
                        }
                    }
                    if account_access {
                        c.account_access = Some(
                            json!({"type":"zhipu-account","accountType":config["access"]["accountType"],"mode":config["access"]["mode"],"entitled":true}),
                        );
                    } else {
                        c.api_key_value = config["access"]["apiKey"].as_str().map(str::to_owned);
                    }
                    prepared.push((
                        (id.clone(), model.clone(), level.clone()),
                        Arc::new(HttpModel::with_pool(c, pool.clone())) as Arc<dyn ModelPort>,
                    ));
                }
                Ok(prepared)
            })();
            // 与 TS 一样，坏的个人模型配置只隔离该候选，不隐藏其它可用模型。
            let Ok(prepared) = prepared else {
                continue;
            };
            result.models.extend(prepared);
            let option = json!({"value":model,"name":model,"modelProviderId":id,"modelProviderName":rule["providerName"].as_str().unwrap_or(&id),"modelThoughtLevels":levels,"contextWindow":mc["properties"]["contextWindow"]});
            result.options.push(option.clone());
            if config["visibility"] != "hidden" {
                result.catalog.push(option);
            }
        }
    }
    let default = &personal["defaultModelSelection"];
    if let Some(option) = result
        .catalog
        .iter()
        .find(|c| c["modelProviderId"] == default["providerId"] && c["value"] == default["modelId"])
        .or_else(|| result.catalog.first())
    {
        let levels = ids(&option["modelThoughtLevels"]);
        let level = default["options"]["reasoningLevel"]
            .as_str()
            .filter(|l| {
                option["modelProviderId"] == default["providerId"]
                    && option["value"] == default["modelId"]
                    && levels.iter().any(|v| v == l)
            })
            .map(str::to_owned)
            .unwrap_or_else(|| levels.last().unwrap().clone());
        result.default = Some(ModelIdentity {
            provider_id: text(option, "modelProviderId").into(),
            model_id: text(option, "value").into(),
            reasoning_level: level,
        });
    }
    Ok(result)
}
