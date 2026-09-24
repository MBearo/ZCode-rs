//! Marketplace snapshots in plugin storage (Node `marketplace.ts`:
//! `loadKnownMarketplacesSync`, `ensureDefaultPluginMarketplaces`,
//! `parseMarketplaceManifest`, `parseEntryStoreListing`) and the effective
//! records of `bootstrap/src/plugins.ts`.
use crate::manifest::{Diagnostic, Severity};
use serde_json::{Map, Value, json};
use std::path::Path;

/// Node `isKnownMarketplaceRecord`.
fn known_record(value: &Value) -> bool {
    value["id"].is_string()
        && value["name"].is_string()
        && value["pluginCount"].is_number()
        && value["source"].is_object()
}

pub async fn known(storage: &Path) -> std::io::Result<Vec<Value>> {
    let value = crate::records::read_storage_json(&storage.join("known_marketplaces.json")).await?;
    let records = match value.as_ref().map(|v| &v["marketplaces"]) {
        Some(Value::Array(items)) => items.iter().filter(|v| known_record(v)).cloned().collect(),
        Some(Value::Object(map)) => map.values().filter(|v| known_record(v)).cloned().collect(),
        _ => vec![],
    };
    Ok(records)
}

/// Node `defaultMarketplaceSourceFromString`.
fn default_source(source: &str) -> Value {
    let trimmed = crate::js::trim(source);
    let shorthand = !trimmed.contains(':')
        && trimmed.split('/').count() == 2
        && trimmed.split('/').all(|part| !part.is_empty());
    if shorthand {
        let cut = trimmed.rfind(['#', '@']).filter(|i| *i > 0);
        return match cut {
            Some(i) => json!({"source":"github","repo":&trimmed[..i],"ref":&trimmed[i + 1..]}),
            None => json!({"source":"github","repo":trimmed}),
        };
    }
    json!({"source":"url","url":trimmed})
}

/// Node `ensureDefaultPluginMarketplaces`: missing default records are
/// appended and the file rewritten in its array form.
pub async fn ensure_defaults(storage: &Path) -> std::io::Result<Vec<Value>> {
    let mut records = known(storage).await?;
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let mut changed = false;
    for default in crate::official::DATA["defaultMarketplaces"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if records.iter().any(|r| r["id"] == default["id"]) {
            continue;
        }
        let mut record = json!({"id":default["id"],"source":default_source(default["source"].as_str().unwrap_or_default()),
            "name":default["name"],"description":default["description"],"addedAt":now,"pluginCount":default["pluginCount"]});
        if let Some(updated) = default.get("lastUpdated") {
            record["lastUpdated"] = updated.clone();
        }
        records.push(record);
        changed = true;
    }
    if changed {
        let text = serde_json::to_string_pretty(&json!({"version":1,"marketplaces":records}))
            .expect("JSON serializes");
        tokio::fs::create_dir_all(storage).await?;
        tokio::fs::write(storage.join("known_marketplaces.json"), format!("{text}\n")).await?;
    }
    Ok(records)
}

/// Node `parseEntryStoreListing`.
pub fn listing(entry: &Value) -> Option<Value> {
    let text = |key: &str| {
        entry[key]
            .as_str()
            .filter(|s| !crate::js::trim(s).is_empty())
            .map(|s| Value::String(s.to_owned()))
    };
    let string_map = |key: &str| {
        let map: Map<String, Value> = entry[key]
            .as_object()?
            .iter()
            .filter(|(_, v)| v.is_string())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        (!map.is_empty()).then_some(Value::Object(map))
    };
    let mut listing = Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            listing.insert(key.to_owned(), value);
        }
    };
    put("displayName", text("displayName"));
    put("displayNameI18n", string_map("displayName_i18n"));
    put("descriptionI18n", string_map("description_i18n"));
    for key in [
        "icon",
        "category",
        "homepage",
        "privacyPolicy",
        "termsOfService",
        "heroImage",
    ] {
        put(key, text(key));
    }
    if let Some((name, url)) = crate::manifest::author(&entry["author"]) {
        put("author", name.map(Value::String));
        put("authorUrl", url.map(Value::String));
    }
    let prompts: Vec<Value> = entry["examplePrompts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v.as_str().is_some_and(|s| !crate::js::trim(s).is_empty()))
        .cloned()
        .collect();
    put(
        "examplePrompts",
        (!prompts.is_empty()).then_some(Value::Array(prompts)),
    );
    let prompts_i18n: Map<String, Value> = entry["examplePrompts_i18n"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(locale, list)| {
            let items: Vec<Value> = list
                .as_array()?
                .iter()
                .filter(|v| v.is_string())
                .cloned()
                .collect();
            (!items.is_empty()).then(|| (locale.clone(), Value::Array(items)))
        })
        .collect();
    put(
        "examplePromptsI18n",
        (!prompts_i18n.is_empty()).then_some(Value::Object(prompts_i18n)),
    );
    if entry["requiresPaidPlan"] == true {
        put("requiresPaidPlan", Some(Value::Bool(true)));
    }
    (!listing.is_empty()).then_some(Value::Object(listing))
}

/// A normalized catalog entry (Node `PluginMarketplaceEntry`).
pub struct Entry {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub source: Option<Value>,
    pub listing: Option<Value>,
    pub raw: Value,
}

pub struct Manifest {
    pub plugins: Vec<Entry>,
    pub featured: Vec<String>,
}

/// Node `parseMarketplaceManifest` + `normalizeMarketplaceManifest`.
pub fn parse_manifest(value: &Value) -> Option<Manifest> {
    let name = crate::js::trim(value["name"].as_str()?);
    if !crate::manifest::valid_name(name) {
        return None;
    }
    let entries: Vec<Value> = match &value["plugins"] {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map
            .iter()
            .map(|(key, plugin)| {
                let mut entry = json!({ "name": key });
                if let Value::Object(fields) = plugin {
                    for (k, v) in fields {
                        entry[k] = v.clone();
                    }
                }
                entry
            })
            .collect(),
        _ => vec![],
    };
    let plugins = entries
        .iter()
        .filter(|entry| entry.is_object())
        .filter_map(|entry| {
            let name = crate::js::trim(entry["name"].as_str().unwrap_or_default());
            (!name.is_empty()).then(|| Entry {
                name: name.to_owned(),
                description: entry["description"].as_str().map(str::to_owned),
                version: entry["version"].as_str().map(str::to_owned),
                source: entry.get("source").cloned(),
                listing: listing(entry),
                raw: entry.clone(),
            })
        })
        .collect();
    let featured = value["featured"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| !crate::js::trim(s).is_empty())
        .map(str::to_owned)
        .collect();
    Some(Manifest { plugins, featured })
}

/// Node `loadMarketplaceManifestSync`: `marketplaces/<id>/marketplace.json`
/// read through the directory's atomic recovery.
pub async fn manifest(storage: &Path, id: &str) -> std::io::Result<Option<Manifest>> {
    let directory = storage
        .join("marketplaces")
        .join(crate::fsx::sanitize_id(id));
    let readable = crate::atomic::recover(&directory).await?;
    let value = crate::fsx::read_json_lenient(&readable.join("marketplace.json")).await;
    Ok(value.as_ref().and_then(parse_manifest))
}

/// Node `readPluginSourceIdentityPin`.
pub fn identity_pin(source: Option<&Value>) -> Option<String> {
    let source = source?.as_object()?;
    let zip = source.get("source").and_then(Value::as_str) == Some("url")
        && source.get("type").and_then(Value::as_str) == Some("zip")
        && source.get("url").is_some_and(Value::is_string);
    if zip && let Some(sha) = source.get("sha256").and_then(Value::as_str) {
        return Some(sha.to_owned());
    }
    ["sha", "commit"]
        .iter()
        .find_map(|key| source.get(*key).and_then(Value::as_str).map(str::to_owned))
}

/// Node `resolveDeclaredMarketplaceSources`: user-declared sources, with
/// relative file and directory paths resolved against the user config file.
fn declared(config: &Value, user_path: &Path) -> Vec<(String, Value)> {
    let base = user_path.parent().unwrap_or(Path::new(""));
    config["plugins"]["extraKnownMarketplaces"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, declaration)| {
            let mut source = declaration["source"].clone();
            if matches!(source["source"].as_str(), Some("file" | "directory"))
                && let Some(path) = source["path"].as_str()
            {
                source["path"] = crate::fsx::resolve(base, path)
                    .to_string_lossy()
                    .into_owned()
                    .into();
            }
            (id.clone(), source)
        })
        .collect()
}

fn declared_record(id: &str, source: Value) -> Value {
    json!({"id":id,"source":source,"name":id,"addedAt":"","pluginCount":0})
}

/// Node `resolveEffectiveMarketplaceRecords`: `(record, use cached manifest)`.
pub fn effective(config: &Value, user_path: &Path, known: &[Value]) -> Vec<(Value, bool)> {
    let declared = declared(config, user_path);
    let mut records: Vec<(Value, bool)> = known
        .iter()
        .map(|record| {
            let id = record["id"].as_str().unwrap_or_default();
            match declared.iter().find(|(d, _)| d == id) {
                // 官方 id 是 Host 保留身份：声明不同来源只产生诊断，仍投影官方缓存。
                Some((_, source))
                    if *source != record["source"] && id != crate::official::MARKETPLACE =>
                {
                    (declared_record(id, source.clone()), false)
                }
                _ => (record.clone(), true),
            }
        })
        .collect();
    for (id, source) in &declared {
        if id != crate::official::MARKETPLACE && !known.iter().any(|r| r["id"] == id.as_str()) {
            records.push((declared_record(id, source.clone()), false));
        }
    }
    records
}

/// Node `resolveMarketplaceDeclarationDiagnostics`.
pub fn declaration_diagnostics(
    config: &Value,
    user_path: &Path,
    known: &[Value],
) -> Vec<Diagnostic> {
    let official = crate::official::MARKETPLACE;
    declared(config, user_path)
        .into_iter()
        .filter(|(id, source)| {
            id == official
                && !known
                    .iter()
                    .any(|r| r["id"] == official && r["source"] == *source)
        })
        .map(|(id, _)| {
            Diagnostic::new(
                "plugin_marketplace_declaration_reserved",
                Severity::Warning,
                format!(
                    "Workspace marketplace declaration \"{id}\" uses a reserved official id and was ignored. Use a different marketplace id for project declarations."
                ),
            )
            .plugin(&id)
        })
        .collect()
}

#[cfg(test)]
#[path = "market_tests.rs"]
mod tests;
