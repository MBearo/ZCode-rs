//! Trusted resolution of a suggested prompt's Plugin before install (Node
//! `plugin-reference-catalog.ts` `resolveSuggestedPluginReference`). The
//! refresh itself is orchestrated by the tools adapter. Spec
//! rust-m10-4-plugin-sources §9.
use crate::official::MARKETPLACE;
use serde_json::{Value, json};

pub const REFRESH_TIMEOUT_MS: u64 = 10_000;

/// A `status: "unavailable"` result with one error diagnostic.
pub fn unavailable(stable_id: &str, code: &str, message: &str) -> Value {
    json!({"stableId":stable_id,"status":"unavailable","diagnostics":[
        {"code":code,"message":message,"severity":"error","pluginId":stable_id}]})
}

pub fn cancelled(stable_id: &str) -> Value {
    unavailable(stable_id, "plugin_operation_cancelled", "插件操作已取消")
}

pub fn refresh_failed(stable_id: &str, message: &str) -> Value {
    unavailable(stable_id, "marketplace_refresh_failed", message)
}

pub fn timeout_message() -> String {
    format!("刷新 {MARKETPLACE} 超时（{REFRESH_TIMEOUT_MS} ms）")
}

fn segment(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// `(plugin name)` of a trusted official stable id, or the untrusted result.
pub fn parse(stable_id: &str) -> Result<String, Value> {
    let at = stable_id.rfind('@').filter(|at| *at > 0);
    let (name, marketplace) = match at {
        Some(at) => (&stable_id[..at], &stable_id[at + 1..]),
        None => ("", ""),
    };
    let shaped = stable_id
        .split_once('@')
        .is_some_and(|(a, b)| segment(a) && segment(b));
    if name.is_empty() || marketplace != MARKETPLACE || !shaped {
        return Err(unavailable(
            stable_id,
            "plugin_suggested_reference_untrusted_source",
            "推荐插件不是受信任的官方 zcode-plugins-official 来源",
        ));
    }
    Ok(name.to_owned())
}

/// Node `toResult`: a catalog entry is ready, disabled or conflicting.
pub fn from_entry(stable_id: &str, name: &str, entry: &Value, icon: Option<&str>) -> Value {
    let conflict = entry["conflictingPluginIds"]
        .as_array()
        .is_some_and(|ids| !ids.is_empty());
    let status = if conflict {
        "conflict"
    } else if entry["enabled"] == true {
        "ready"
    } else {
        "disabled"
    };
    let mut result = json!({"stableId":stable_id,"status":status,"marketplace":MARKETPLACE,
        "pluginName":name,"sourceTrust":"official","diagnostics":[]});
    if let Some(icon) = icon.filter(|i| !i.is_empty()) {
        result["icon"] = icon.into();
    }
    if conflict {
        result["diagnostics"] = json!([{"code":"plugin_suggested_reference_conflict",
            "message":"推荐插件存在同名冲突，不能自动安装或引用","severity":"error","pluginId":stable_id}]);
    }
    result
}

/// After a refresh without a local entry: a listed official candidate is
/// `missing` (installable), anything else is not listed.
pub fn from_overview(stable_id: &str, name: &str, overview: &Value) -> Value {
    let candidate = overview["availablePlugins"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == stable_id)
        .filter(|p| p["name"] == name && p["marketplace"] == MARKETPLACE);
    let Some(candidate) = candidate else {
        return unavailable(
            stable_id,
            "plugin_suggested_reference_not_listed",
            "刷新后的官方目录中未找到该插件",
        );
    };
    let mut result = json!({"stableId":stable_id,"status":"missing","marketplace":MARKETPLACE,
        "pluginName":candidate["name"],"sourceTrust":"official","diagnostics":[]});
    if let Some(icon) = candidate["listing"]["icon"]
        .as_str()
        .map(crate::js::trim)
        .filter(|i| !i.is_empty())
    {
        result["icon"] = icon.into();
    }
    if candidate["listing"].is_object() {
        result["listing"] = candidate["listing"].clone();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_references_follow_node() {
        let id = format!("demo@{MARKETPLACE}");
        assert_eq!(parse(&id), Ok("demo".to_owned()));
        let untrusted = parse("demo@other").unwrap_err();
        assert_eq!(
            untrusted["diagnostics"][0]["code"],
            "plugin_suggested_reference_untrusted_source"
        );
        assert!(parse(&format!("a@b@{MARKETPLACE}")).is_err());
        let entry = json!({"enabled":false,"conflictingPluginIds":[]});
        assert_eq!(
            from_entry(&id, "demo", &entry, Some("i"))["status"],
            "disabled"
        );
        let conflict = json!({"enabled":true,"conflictingPluginIds":["x"]});
        let result = from_entry(&id, "demo", &conflict, None);
        assert_eq!(result["status"], "conflict");
        assert!(result.get("icon").is_none());
        let overview = json!({"availablePlugins":[{"id":id,"name":"demo","marketplace":MARKETPLACE,
            "listing":{"icon":" i ","category":"dev"}}]});
        let missing = from_overview(&id, "demo", &overview);
        assert_eq!(
            (missing["status"].as_str(), missing["icon"].as_str()),
            (Some("missing"), Some("i"))
        );
        assert_eq!(
            from_overview(&id, "demo", &json!({}))["diagnostics"][0]["code"],
            "plugin_suggested_reference_not_listed"
        );
        assert_eq!(
            timeout_message(),
            format!("刷新 {MARKETPLACE} 超时（10000 ms）")
        );
    }
}
