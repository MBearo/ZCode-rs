//! Plugin storage layout and installed records (Node `marketplace.ts`
//! `listInstalledPluginRecords`, `resolveInstalledPluginRoot` and
//! `official-marketplace.ts` `loadBundledOfficialPluginRootsSync`).
use crate::fsx::{normalize, sanitize_id};
use crate::manifest::{DEFAULT_VERSION, Diagnostic, Severity};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Node `getPluginStorageRoot(getCliStorageRoot(resolvePath(storage.dir)))`;
/// `ZCODE_STORAGE_DIR` reaches `storage.dir` through the config env layer.
pub fn storage_root(config: &Value, home: &Path) -> PathBuf {
    let base = config["storage"]["dir"].as_str().map(str::to_owned);
    let base = match base {
        Some(dir) => match dir.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None if dir == "~" => home.to_owned(),
            None => normalize(&home.join(dir)),
        },
        None => home.join(".zcode"),
    };
    if base.file_name().is_some_and(|name| name == "cli") {
        base.join("plugins")
    } else {
        base.join("cli").join("plugins")
    }
}

/// Node `InstalledPluginRecord`.
#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub marketplace: String,
    pub version: String,
    pub install_path: String,
    pub installed_at: String,
    pub updated_at: Option<String>,
    pub workspace_scope: bool,
    pub source: Option<Value>,
    /// The stored record (normalized for the map form), written back on save.
    pub raw: Value,
}

impl Installed {
    pub fn scope(&self) -> &'static str {
        if self.workspace_scope {
            "workspace"
        } else {
            "user"
        }
    }
}

/// Node `parsePluginId`: `(name, marketplace)` split at the last `@`.
pub fn split_id(id: &str) -> Option<(&str, &str)> {
    let at = id.rfind('@')?;
    (at > 0 && at < id.len() - 1).then(|| (&id[..at], &id[at + 1..]))
}

fn from_array(value: &Value) -> Option<Installed> {
    let text = |key: &str| value[key].as_str().map(str::to_owned);
    let scope = value["scope"].as_str()?;
    if scope != "user" && scope != "workspace" {
        return None;
    }
    Some(Installed {
        id: text("id")?,
        name: text("name")?,
        marketplace: text("marketplace")?,
        version: text("version")?,
        install_path: text("installPath")?,
        installed_at: text("installedAt")?,
        updated_at: text("updatedAt"),
        workspace_scope: scope == "workspace",
        source: value.get("source").cloned(),
        raw: value.clone(),
    })
}

fn from_map(id: &str, entry: &Value) -> Vec<Installed> {
    let entries = match entry {
        Value::Array(items) => items.clone(),
        other => vec![other.clone()],
    };
    let Some((name, marketplace)) = split_id(id) else {
        return vec![];
    };
    entries
        .iter()
        .filter_map(|item| {
            let install_path = item["installPath"].as_str().filter(|p| !p.is_empty())?;
            let workspace_scope = matches!(item["scope"].as_str(), Some("project" | "local"));
            let version = item["version"].as_str().unwrap_or(DEFAULT_VERSION);
            let installed_at = item["installedAt"]
                .as_str()
                .unwrap_or("1970-01-01T00:00:00.000Z");
            let updated_at = item["lastUpdated"].as_str().map(str::to_owned);
            let mut raw = serde_json::json!({"id":id,"name":name,"marketplace":marketplace,
                "version":version,"installPath":install_path,"installedAt":installed_at,
                "scope":if workspace_scope {"workspace"} else {"user"}});
            if let Some(updated) = &updated_at {
                raw["updatedAt"] = updated.clone().into();
            }
            Some(Installed {
                id: id.to_owned(),
                name: name.to_owned(),
                marketplace: marketplace.to_owned(),
                version: version.to_owned(),
                install_path: install_path.to_owned(),
                installed_at: installed_at.to_owned(),
                updated_at,
                workspace_scope,
                source: None,
                raw,
            })
        })
        .collect()
}

/// Node `normalizeInstalledPluginsState`.
pub fn parse_installed(value: &Value) -> Vec<Installed> {
    match &value["plugins"] {
        Value::Object(map) => map.iter().flat_map(|(id, e)| from_map(id, e)).collect(),
        Value::Array(items) => items.iter().filter_map(from_array).collect(),
        _ => vec![],
    }
}

/// A storage JSON file read through atomic recovery (Node `readJsonFileSync`).
pub async fn read_storage_json(path: &Path) -> std::io::Result<Option<Value>> {
    let readable = crate::atomic::recover(path).await?;
    Ok(crate::fsx::read_json_lenient(&readable).await)
}

pub async fn installed(storage: &Path) -> std::io::Result<Vec<Installed>> {
    let value = read_storage_json(&storage.join("installed_plugins.json")).await?;
    Ok(value.as_ref().map(parse_installed).unwrap_or_default())
}

/// Node `saveInstalledPlugins`: `{version: 1, plugins: [...]}`, atomically.
pub async fn save_installed(storage: &Path, records: &[Installed]) -> std::io::Result<()> {
    let plugins: Vec<&Value> = records.iter().map(|r| &r.raw).collect();
    let text = serde_json::to_string_pretty(&serde_json::json!({"version":1,"plugins":plugins}))
        .expect("JSON serializes");
    crate::atomic_write::write_file(
        &storage.join("installed_plugins.json"),
        format!("{text}\n").as_bytes(),
    )
    .await
}

/// Node `resolveInstalledPluginRoot`.
pub async fn installed_root(storage: &Path, record: &Installed) -> std::io::Result<PathBuf> {
    let root = if record.install_path.is_empty() {
        storage
            .join("cache")
            .join(sanitize_id(&record.marketplace))
            .join(sanitize_id(&record.name))
            .join(sanitize_id(&record.version))
    } else {
        normalize(&storage.join(&record.install_path))
    };
    crate::atomic::recover(&root).await
}

fn strict_descendant(parent: &Path, child: &Path) -> bool {
    child != parent && child.starts_with(parent)
}

/// Node `loadBundledOfficialPluginRootsSync`: `None` without a valid bundled
/// partition; cache paths outside `cache/zcode-plugins-official/<name>/` are dropped.
pub async fn bundled_roots(storage: &Path) -> Option<Vec<PathBuf>> {
    let official = crate::official::MARKETPLACE;
    let path = storage
        .join("marketplaces")
        .join(official)
        .join("bundled-marketplace.json");
    let partition = crate::fsx::read_json_lenient(&path).await?;
    if partition["version"] != 1 || !partition["manifest"].is_object() {
        return None;
    }
    let cache_root = normalize(&storage.join("cache").join(official));
    let roots = partition["manifest"]["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|plugin| {
            let name = plugin["name"].as_str().filter(|n| !n.is_empty())?;
            let cache_path = normalize(&std::path::absolute(plugin["cachePath"].as_str()?).ok()?);
            let plugin_root = normalize(&cache_root.join(name));
            (strict_descendant(&cache_root, &plugin_root)
                && strict_descendant(&plugin_root, &cache_path))
            .then_some(cache_path)
        })
        .collect();
    Some(roots)
}

/// Node `scanOfficialCache`: the bundled roots, else every version directory
/// under `cache/zcode-plugins-official/*/`.
pub async fn official_roots(storage: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    if let Some(roots) = bundled_roots(storage).await {
        return roots;
    }
    let cache = storage.join("cache").join(crate::official::MARKETPLACE);
    let scan = async {
        let mut roots = vec![];
        for (plugin, kind) in crate::fsx::read_dir_names(&cache).await? {
            if !kind.is_dir() {
                continue;
            }
            for (version, kind) in crate::fsx::read_dir_names(&cache.join(&plugin)).await? {
                if kind.is_dir() {
                    roots.push(cache.join(&plugin).join(version));
                }
            }
        }
        Ok::<_, std::io::Error>(roots)
    };
    match scan.await {
        Ok(roots) => roots,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(error) => {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_root_not_found",
                    Severity::Warning,
                    error.to_string(),
                )
                .at(cache),
            );
            vec![]
        }
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
