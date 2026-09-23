use super::extension_config::{json_file, resolve, storage, strings};
use anyhow::Result;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub(super) struct Plugin {
    pub id: String,
    pub root: PathBuf,
    pub name: String,
    pub manifest: Value,
}
pub(super) async fn enabled(
    cwd: &Path,
    config: &Value,
    cancel: &CancellationToken,
) -> Result<Vec<Plugin>> {
    if config["plugins"]["enabled"] == false {
        return Ok(vec![]);
    }
    let storage = storage(config);
    let mut candidates = strings(&config["plugins"]["dirs"])
        .into_iter()
        .map(|p| (resolve(cwd, p), "inline".to_owned(), true, false))
        .collect::<Vec<_>>();
    let official = "zcode-plugins-official";
    let partition =
        json_file(&storage.join(format!("marketplaces/{official}/bundled-marketplace.json")))
            .await?;
    if let Some(entries) = partition["manifest"]["plugins"].as_array() {
        for entry in entries {
            if let (Some(path), Some(name)) = (entry["cachePath"].as_str(), entry["name"].as_str())
            {
                let root = resolve(&storage, path);
                let boundary = storage.join("cache").join(official).join(name);
                if root.starts_with(&boundary) && root != boundary {
                    candidates.push((root, official.into(), false, true));
                }
            }
        }
    } else {
        for plugin in directories(&storage.join("cache").join(official)).await? {
            for version in directories(&plugin).await? {
                candidates.push((version, official.into(), false, true));
            }
        }
    }
    let installed = json_file(&storage.join("installed_plugins.json")).await?;
    let mut records = vec![];
    if let Some(entries) = installed["plugins"].as_array() {
        records.extend(entries.iter().cloned());
    }
    if let Some(entries) = installed["plugins"].as_object() {
        for (id, value) in entries {
            for value in value
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![value.clone()])
            {
                if let Some((name, market)) = id.rsplit_once('@') {
                    let mut value = value;
                    value["name"] = name.into();
                    value["marketplace"] = market.into();
                    records.push(value);
                }
            }
        }
    }
    for record in records {
        if let (Some(name), Some(market)) =
            (record["name"].as_str(), record["marketplace"].as_str())
        {
            let root = record["installPath"]
                .as_str()
                .map(|p| resolve(&storage, p))
                .unwrap_or_else(|| {
                    storage
                        .join("cache")
                        .join(market)
                        .join(name)
                        .join(record["version"].as_str().unwrap_or("0.0.0"))
                });
            candidates.push((root, market.into(), false, false));
        }
    }
    let defaults: Vec<String> = serde_json::from_str(include_str!("plugin_defaults.json"))?;
    let mut seen = BTreeSet::new();
    let mut plugins = vec![];
    for (root, market, default, official_source) in candidates {
        super::tools::check_cancel(cancel)?;
        let mut manifest = Value::Null;
        for dir in [".zcode-plugin", ".claude-plugin", ".codex-plugin"] {
            let found = json_file(&root.join(dir).join("plugin.json")).await?;
            if found["name"].is_string() {
                manifest = found;
                break;
            }
        }
        let Some(name) = manifest["name"].as_str().filter(|n| {
            !n.is_empty()
                && n.len() <= 128
                && n.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        }) else {
            continue;
        };
        let id = format!("{name}@{market}");
        if official_source
            && strings(&config["plugins"]["suppressedBuiltins"]).contains(&id.as_str())
        {
            continue;
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if !config["plugins"]["enabledPlugins"][&id]
            .as_bool()
            .unwrap_or(default || defaults.contains(&id))
        {
            continue;
        }
        plugins.push(Plugin {
            id,
            root,
            name: name.into(),
            manifest,
        });
    }
    Ok(plugins)
}
pub(super) async fn directories(root: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut dirs = vec![];
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}
pub(super) async fn contained_file(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_owned();
    for part in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(part) = part {
            current.push(part);
        }
        if !tokio::fs::symlink_metadata(&current)
            .await
            .is_ok_and(|m| !m.file_type().is_symlink())
        {
            return false;
        }
    }
    true
}
