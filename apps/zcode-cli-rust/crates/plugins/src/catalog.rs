//! Plugin reference catalog (Node `buildPluginReferenceCatalog` and the
//! `plugins/referenceCatalog` projection). Spec rust-m10-plugins §3.7.
use crate::discovery::Plugin;
use serde_json::{Map, Value, json};

fn qualified(plugin: &Plugin, kind: &str) -> Vec<String> {
    let mut names: Vec<String> = plugin
        .components
        .iter()
        .filter(|g| g.kind == kind)
        .flat_map(|g| g.items.iter())
        .map(|(name, _)| crate::js::trim(name))
        .filter(|name| !name.is_empty())
        .map(|name| format!("{}:{name}", plugin.loaded.name()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Node `buildPluginReferenceCatalog`: every discovered plugin; enabled
/// plugins sharing a manifest name conflict with each other.
pub fn build(plugins: &[Plugin]) -> Vec<Value> {
    plugins
        .iter()
        .map(|plugin| {
            let mut conflicts: Vec<&str> = if plugin.enabled {
                plugins
                    .iter()
                    .filter(|p| p.enabled && p.loaded.name() == plugin.loaded.name())
                    .map(|p| p.loaded.id.as_str())
                    .filter(|id| *id != plugin.loaded.id)
                    .collect()
            } else {
                vec![]
            };
            conflicts.sort();
            let mut servers = plugin.mcp_server_names.clone();
            servers.sort();
            json!({"pluginId":plugin.loaded.id,"name":plugin.loaded.name(),
                "marketplace":plugin.loaded.marketplace,"enabled":plugin.enabled,
                "conflictingPluginIds":conflicts,"skillQualifiedNames":qualified(plugin, "skill"),
                "mcpServerNames":servers,"subagentNames":qualified(plugin, "agent"),
                "rootPath":plugin.loaded.root})
        })
        .collect()
}

fn trimmed(value: &Value) -> Option<Value> {
    value
        .as_str()
        .map(crate::js::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_owned()))
}

/// Node `resolveReferenceListingDisplayByPluginId` over an overview result.
pub fn display(overview: &Value) -> Map<String, Value> {
    let mut display = Map::new();
    for section in ["availablePlugins", "installedPlugins", "restorableBuiltins"] {
        for plugin in overview[section].as_array().into_iter().flatten() {
            let listing = &plugin["listing"];
            let mut fields = Map::new();
            let mut put = |key: &str, value: Option<Value>| {
                if let Some(value) = value {
                    fields.insert(key.into(), value);
                }
            };
            put("category", trimmed(&listing["category"]));
            put("icon", trimmed(&listing["icon"]));
            put("displayName", trimmed(&listing["displayName"]));
            put("displayNameI18n", listing.get("displayNameI18n").cloned());
            put("description", trimmed(&plugin["description"]));
            put("descriptionI18n", listing.get("descriptionI18n").cloned());
            if fields.is_empty() {
                continue;
            }
            let id = plugin["id"].as_str().unwrap_or_default().to_owned();
            let entry = display.entry(id).or_insert_with(|| json!({}));
            for (key, value) in fields {
                entry[key] = value;
            }
        }
    }
    display
}

/// Node `toReferenceCatalogEntry`: identity fields plus display, never the root path.
pub fn project(entry: &Value, display: &Map<String, Value>, category: bool) -> Value {
    let shown = display.get(entry["pluginId"].as_str().unwrap_or_default());
    let mut out = json!({"pluginId":entry["pluginId"],"name":entry["name"],"marketplace":entry["marketplace"],
        "enabled":entry["enabled"],"conflictingPluginIds":entry["conflictingPluginIds"],
        "skillQualifiedNames":entry["skillQualifiedNames"],"mcpServerNames":entry["mcpServerNames"],
        "subagentNames":entry["subagentNames"]});
    if category {
        out["category"] = shown
            .and_then(|d| d.get("category").cloned())
            .unwrap_or_else(|| "other".into());
    }
    for key in [
        "icon",
        "displayName",
        "displayNameI18n",
        "description",
        "descriptionI18n",
    ] {
        if let Some(value) = shown.and_then(|d| d.get(key)) {
            out[key] = value.clone();
        }
    }
    out
}
