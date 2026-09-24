//! `plugins/overview` (Node `getZCodePluginsOverview` and the protocol
//! projections of `zcode-protocol/plugins.ts`).
use crate::discovery::{Outcome, Plugin};
use crate::manifest::Diagnostic;
use serde_json::{Map, Value, json};
use std::path::Path;

pub struct Input<'a> {
    pub config: &'a Value,
    pub storage: &'a Path,
    pub user_path: &'a Path,
    pub env: &'a (dyn Fn(&str) -> Option<String> + Sync),
}

/// Node `isZCodeCuaInternalFeatureEnabled`.
fn cua_enabled(env: &(dyn Fn(&str) -> Option<String> + Sync)) -> bool {
    let flag = |key: &str| env(key).map(|v| crate::js::trim(&v).to_lowercase());
    if flag("ZCODE_CUA_DEV_MODE").is_some_and(|v| matches!(v.as_str(), "1" | "true" | "on")) {
        return true;
    }
    !flag("ZCODE_CUA_PRODUCT_HELPER").is_some_and(|v| matches!(v.as_str(), "0" | "false" | "off"))
}

/// Node `inferComponentTypes` over a catalog entry.
fn entry_types(raw: &Value) -> Vec<&'static str> {
    [
        ("agents", "agent"),
        ("commands", "command"),
        ("skills", "skill"),
        ("hooks", "hook"),
        ("mcpServers", "mcp"),
        ("lspServers", "lsp"),
    ]
    .into_iter()
    .filter(|(key, _)| raw.get(*key).is_some())
    .map(|(_, kind)| kind)
    .collect()
}

/// Node `inferComponentTypesFromMetadata`.
pub fn plugin_types(plugin: &Plugin) -> Vec<&'static str> {
    let mut types = vec![];
    if plugin
        .components
        .iter()
        .any(|g| g.kind == "agent" && !g.items.is_empty())
    {
        types.push("agent");
    }
    if plugin.command_root_count > 0 {
        types.push("command");
    }
    if plugin.skill_root_count > 0 || plugin.skill_count > 0 {
        types.push("skill");
    }
    if !plugin.declared_mcp.is_empty() || !plugin.mcp_server_names.is_empty() {
        types.push("mcp");
    }
    if !plugin.hook_details.is_empty() {
        types.push("hook");
    }
    types
}

fn put(object: &mut Value, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        object[key] = value;
    }
}

fn non_empty(value: Option<&str>) -> Option<Value> {
    value
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_owned()))
}

fn marketplace_summary(record: &Value, featured: &[String], count: Option<usize>) -> Value {
    let id = record["id"].as_str().unwrap_or_default();
    let mut summary = json!({"id":id,"name":record["name"],"source":record["source"],
        "pluginCount":count.map(Value::from).unwrap_or_else(|| record["pluginCount"].clone()),
        "isOfficial":id == crate::official::MARKETPLACE});
    put(
        &mut summary,
        "description",
        non_empty(record["description"].as_str()),
    );
    put(
        &mut summary,
        "lastUpdated",
        non_empty(record["lastUpdated"].as_str()),
    );
    if let Some(failure) = record["lastRefreshFailure"].as_object() {
        summary["refreshFailure"] = json!({"code":failure.get("code"),"failedAt":failure.get("failedAt"),
            "message":failure.get("message")});
    }
    if !featured.is_empty() {
        summary["featured"] = json!(featured);
    }
    summary
}

/// Node `getZCodePluginsOverview` projected to `ZCodePluginsOverviewResult`.
pub async fn overview(input: &Input<'_>, outcome: &Outcome) -> std::io::Result<Value> {
    let known = crate::market::ensure_defaults(input.storage).await?;
    let effective = crate::market::effective(input.config, input.user_path, &known);
    let installed = crate::records::installed(input.storage).await?;
    let mut marketplaces = vec![];
    let mut available = vec![];
    let mut pins: Map<String, Value> = Map::new();
    let mut listings: Map<String, Value> = Map::new();
    for (record, cached) in &effective {
        let id = record["id"].as_str().unwrap_or_default();
        let manifest = if *cached {
            crate::market::manifest(input.storage, id).await?
        } else {
            None
        };
        let entries = manifest.as_ref().map_or(&[][..], |m| m.plugins.as_slice());
        // node-repl-host 是官方运行时宿主，不计入可见插件数。
        let count = manifest.as_ref().map(|m| {
            m.plugins
                .iter()
                .filter(|e| {
                    id != crate::official::MARKETPLACE
                        || e.name != crate::official::DATA["nodeReplHost"]
                })
                .count()
        });
        let featured = manifest.as_ref().map_or(&[][..], |m| m.featured.as_slice());
        marketplaces.push(marketplace_summary(record, featured, count));
        for entry in entries {
            let plugin_id = format!("{}@{id}", entry.name);
            let mut item = json!({"id":plugin_id,"name":entry.name,"marketplace":id,
                "installed":installed.iter().any(|r| r.id == plugin_id),"componentTypes":entry_types(&entry.raw)});
            put(
                &mut item,
                "description",
                non_empty(entry.description.as_deref()),
            );
            put(&mut item, "version", non_empty(entry.version.as_deref()));
            put(&mut item, "listing", entry.listing.clone());
            let mut pin = json!({});
            put(&mut pin, "version", non_empty(entry.version.as_deref()));
            put(
                &mut pin,
                "sha",
                crate::market::identity_pin(entry.source.as_ref()).map(Value::String),
            );
            pins.insert(plugin_id.clone(), pin);
            if let Some(listing) = &entry.listing {
                listings.insert(plugin_id, listing.clone());
            }
            available.push(item);
        }
    }
    let suppressed = crate::fsx::path_list(&input.config["plugins"]["suppressedBuiltins"]);
    let restorable: Vec<Value> = crate::official::definitions()
        .iter()
        .filter(|d| {
            let name = d["name"].as_str().unwrap_or_default();
            suppressed.contains(&format!("{name}@{}", crate::official::MARKETPLACE))
                && (name != "computer-use" || cua_enabled(input.env))
        })
        .map(|d| {
            let mut item = json!({"id":format!("{}@{}", d["name"].as_str().unwrap_or_default(), crate::official::MARKETPLACE),
                "name":d["name"],"marketplace":crate::official::MARKETPLACE,"installed":false});
            put(&mut item, "version", non_empty(d["version"].as_str()));
            put(&mut item, "listing", d.get("listing").cloned());
            item
        })
        .collect();
    let installed_items: Vec<Value> = installed
        .iter()
        .map(|record| {
            let loaded = outcome.plugins.iter().find(|p| p.loaded.id == record.id);
            let enabled = input.config["plugins"]["enabledPlugins"][&record.id]
                .as_bool()
                .unwrap_or(false);
            let version = loaded
                .and_then(|p| p.loaded.manifest["version"].as_str())
                .unwrap_or(&record.version);
            let mut item = json!({"id":record.id,"name":record.name,"marketplace":record.marketplace,
                "enabled":enabled,"scope":record.scope()});
            put(&mut item, "description", non_empty(loaded.and_then(|p| p.loaded.manifest["description"].as_str())));
            put(&mut item, "version", non_empty(Some(version)));
            put(&mut item, "installPath", non_empty(Some(&record.install_path)));
            put(&mut item, "installedAt", non_empty(Some(&record.installed_at)));
            if let Some(plugin) = loaded {
                item["componentTypes"] = json!(plugin_types(plugin));
                item["hookDetails"] = json!(plugin.hook_details);
            }
            let pin = pins.get(&record.id);
            let latest_version = pin.and_then(|p| p["version"].as_str());
            let latest_sha = pin.and_then(|p| p["sha"].as_str());
            let installed_sha = crate::market::identity_pin(record.source.as_ref());
            item["updateStatus"] = crate::version::compare_update(
                Some(version),
                installed_sha.as_deref(),
                latest_version,
                latest_sha,
            )
            .into();
            let label = latest_version
                .map(str::to_owned)
                .or_else(|| latest_sha.map(|sha| sha.chars().take(7).collect()));
            put(&mut item, "latestVersion", non_empty(label.as_deref()));
            put(&mut item, "listing", listings.get(&record.id).cloned());
            item
        })
        .collect();
    let mut diagnostics: Vec<Value> = outcome
        .diagnostics
        .iter()
        .map(Diagnostic::protocol)
        .collect();
    diagnostics.extend(
        crate::market::declaration_diagnostics(input.config, input.user_path, &known)
            .iter()
            .map(Diagnostic::protocol),
    );
    for record in &known {
        if let Some(failure) = record["lastRefreshFailure"].as_object() {
            diagnostics.push(
                json!({"code":failure.get("code"),"message":failure.get("message"),
                "pluginId":record["id"],"severity":"error"}),
            );
        }
    }
    Ok(
        json!({"marketplaces":marketplaces,"availablePlugins":available,"installedPlugins":installed_items,
        "restorableBuiltins":restorable,"diagnostics":diagnostics,"capability":{"supported":true}}),
    )
}
