use anyhow::Result;
use serde_json::{Value, json};
pub(super) fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub(super) fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
// Config children overlay recursively; dictionaries such as headers and leaf maps replace wholesale.
pub(super) fn overlay(base: &mut Value, next: &Value) {
    let Some(o) = next.as_object() else {
        *base = next.clone();
        return;
    };
    if !base.is_object() {
        *base = json!({});
    }
    for (k, v) in o {
        if [
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
        .contains(&k.as_str())
            && v.is_object()
        {
            // access type 是 discriminant，不能继承另一种认证方式的秘密字段。
            if k == "access" && base[k]["type"] != v["type"] {
                base[k] = v.clone();
            } else {
                overlay(&mut base[k], v);
            }
        } else {
            base[k] = v.clone();
        }
    }
}
pub(super) fn order(builtin: Vec<String>, personal: Vec<String>, requested: &Value) -> Vec<String> {
    let requested = array(requested)
        .iter()
        .filter_map(Value::as_str)
        .filter(|id| builtin.iter().chain(&personal).any(|v| v == id))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut result = vec![];
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
pub(super) fn ids(v: &Value) -> Vec<String> {
    array(v)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
pub(super) struct Rule {
    pub(super) value: Value,
    pub(super) manual: bool,
    pub(super) model: Option<regex::Regex>,
    pub(super) api: Option<regex::Regex>,
    pub(super) url: Option<regex::Regex>,
}
pub(super) fn rules(config: &Value, personal: bool) -> Result<Vec<Rule>> {
    let mut result = vec![];
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
    for group in groups {
        for rule in array(&config[*group]) {
            let regex = |key: &str, insensitive: bool| -> Result<Option<regex::Regex>> {
                rule[key]
                    .as_str()
                    .map(|pattern| {
                        Ok(regex::RegexBuilder::new(&format!("^(?:{pattern})$"))
                            .case_insensitive(insensitive)
                            .build()?)
                    })
                    .transpose()
            };
            result.push(Rule {
                value: rule.clone(),
                manual: *group == "manualProviderModelRules",
                model: regex("modelMatch", true)?,
                api: regex("apiTypeMatch", false)?,
                url: regex("baseUrlMatch", false)?,
            });
        }
    }
    Ok(result)
}

// 手动规则只清除 TS 产品开放的叶子，系统参数映射必须继续继承。
pub(super) fn clear_manual(value: &mut Value) {
    for key in [
        "contextWindow",
        "supportsJsonSchemaOutput",
        "supportsNativeWebSearch",
        "supportsMidConversationSystem",
    ] {
        if let Some(o) = value["properties"].as_object_mut() {
            o.remove(key);
        }
    }
    for key in ["supportsImage", "supportsVideo", "supportsPdf"] {
        if let Some(o) = value["properties"]["inputFormat"].as_object_mut() {
            o.remove(key);
        }
    }
    if let Some(o) = value["optionSpecs"].as_object_mut() {
        o.remove("reasoningLevel");
    }
    if let Some(o) = value["optionSpecs"]["maxOutputTokens"].as_object_mut() {
        o.remove("max");
    }
}
pub(super) fn valid_model(v: &Value) -> bool {
    let p = &v["properties"];
    [
        "requiresMfjsToolSchema",
        "supportsToolCall",
        "supportsJsonSchemaOutput",
        "supportsNativeWebSearch",
        "supportsMidConversationSystem",
    ]
    .iter()
    .all(|k| p[*k].is_boolean())
        && [
            "supportsText",
            "supportsImage",
            "supportsVideo",
            "supportsAudio",
            "supportsPdf",
        ]
        .iter()
        .all(|k| p["inputFormat"][*k].is_boolean())
        && p["outputFormat"]["supportsText"].is_boolean()
        && v["optionSpecs"]["reasoningLevel"]["values"]
            .as_array()
            .is_some_and(|a| {
                !a.is_empty()
                    && a.iter()
                        .all(|s| s.as_str().is_some_and(|s| !s.trim().is_empty()))
                    && a.iter()
                        .filter_map(Value::as_str)
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        == a.len()
            })
}
