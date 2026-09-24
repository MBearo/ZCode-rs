//! Plugin sections of a config file (Node `file-config.adapter.ts`:
//! `updatePluginEnabledInFileConfig`, `updatePluginOptionsInFileConfig`,
//! `removePluginFromFileConfig`, `removePluginEnabledFromFileConfig`,
//! `addSuppressedBuiltinInFileConfig`, `removeSuppressedBuiltinInFileConfig`).
//! Files keep their key order and are replaced atomically (0600).
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use zcode_cli_domain::json_order::Ordered;

const LEGACY_CUA: &str = "zcode-cua@zcode-plugins-official";
const CANONICAL_CUA: &str = "computer-use@zcode-plugins-official";

/// Node `canonicalizePluginId`.
pub fn canonical(id: &str) -> &str {
    if id == LEGACY_CUA { CANONICAL_CUA } else { id }
}

/// Node `pluginIdAliases`.
fn aliases(id: &str) -> Vec<&str> {
    if canonical(id) == CANONICAL_CUA {
        vec![CANONICAL_CUA, LEGACY_CUA]
    } else {
        vec![id]
    }
}

/// Node `readJsonConfigFileOrEmpty`.
async fn read(path: &Path) -> Result<Ordered> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ordered::Object(vec![])),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Unable to read config file: {}", path.display()));
        }
    };
    let tree: Ordered = serde_json::from_str(&text)
        .with_context(|| format!("Unable to parse config file as JSON: {}", path.display()))?;
    if !matches!(tree, Ordered::Object(_)) {
        bail!("Config file must contain a JSON object: {}", path.display());
    }
    Ok(tree)
}

/// Node `atomicWriteJson`: a same-directory 0600 temp file renamed over the target.
async fn write(path: &Path, tree: &Ordered) -> Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(directory).await?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = directory.join(format!(
        ".{name}.{}.{}.{}.tmp",
        std::process::id(),
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4().simple()
    ));
    let content = format!("{}\n", tree.pretty());
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temp).await?;
        file.write_all(content.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error)
            .with_context(|| format!("Unable to write config file: {}", path.display()));
    }
    Ok(())
}

/// `parsed.plugins` as an object, created when missing or not an object.
fn plugins(tree: &mut Ordered) -> &mut Ordered {
    if !matches!(tree.get_mut("plugins"), Some(Ordered::Object(_))) {
        tree.set("plugins", Ordered::Object(vec![]));
    }
    tree.get_mut("plugins").expect("plugins object")
}

fn section<'a>(plugins: &'a mut Ordered, key: &str) -> &'a mut Ordered {
    if !matches!(plugins.get_mut(key), Some(Ordered::Object(_))) {
        plugins.set(key, Ordered::Object(vec![]));
    }
    plugins.get_mut(key).expect("section object")
}

/// Node `patchPluginEnabled`: aliases removed, the canonical id set last.
pub async fn set_enabled(path: &Path, id: &str, enabled: bool) -> Result<()> {
    let mut tree = read(path).await?;
    let enabled_plugins = section(plugins(&mut tree), "enabledPlugins");
    for alias in aliases(canonical(id)) {
        enabled_plugins.remove(alias);
    }
    enabled_plugins.set(canonical(id), Ordered::Bool(enabled));
    write(path, &tree).await
}

/// Node `patchPluginOptions`: stored keys kept unless cleared, then merged.
pub async fn update_options(
    path: &Path,
    id: &str,
    values: &Map<String, Value>,
    clear: &[String],
) -> Result<()> {
    let mut tree = read(path).await?;
    let options = section(plugins(&mut tree), "options");
    let canonical_id = canonical(id);
    let ids = aliases(canonical_id);
    let current = options
        .get_mut(canonical_id)
        .filter(|v| matches!(v, Ordered::Object(_)))
        .cloned()
        .or_else(|| {
            ids.get(1)
                .and_then(|legacy| options.get_mut(legacy))
                .filter(|v| matches!(v, Ordered::Object(_)))
                .cloned()
        })
        .unwrap_or(Ordered::Object(vec![]));
    for alias in &ids {
        options.remove(alias);
    }
    let mut merged = current;
    for key in clear {
        merged.remove(key);
    }
    for (key, value) in values {
        merged.set(key, Ordered::from_value(value));
    }
    options.set(canonical_id, merged);
    write(path, &tree).await
}

/// Node `removePluginFromFileConfig`: enabled flag and options; written only
/// when something was removed.
pub async fn remove_plugin(path: &Path, id: &str) -> Result<()> {
    let mut tree = read(path).await?;
    let has = |tree: &mut Ordered, key: &str| {
        matches!(
            tree.get_mut("plugins").and_then(|p| p.get_mut(key)),
            Some(Ordered::Object(_))
        )
    };
    let (had_enabled, had_options) = (has(&mut tree, "enabledPlugins"), has(&mut tree, "options"));
    let plugins = plugins(&mut tree);
    let mut removed = false;
    for key in ["enabledPlugins", "options"] {
        if let Some(object) = plugins
            .get_mut(key)
            .filter(|v| matches!(v, Ordered::Object(_)))
        {
            for alias in aliases(id) {
                removed |= object.remove(alias);
            }
        }
    }
    if !removed {
        return Ok(());
    }
    // Node 会把缺失或非对象的 enabledPlugins/options 写成空对象。
    for (key, had) in [("enabledPlugins", had_enabled), ("options", had_options)] {
        if !had {
            plugins.set(key, Ordered::Object(vec![]));
        }
    }
    write(path, &tree).await
}

/// Node `removePluginEnabledFromFileConfig`: only the enabled override.
pub async fn remove_enabled(path: &Path, id: &str) -> Result<()> {
    let mut tree = read(path).await?;
    let Some(enabled) = tree
        .get_mut("plugins")
        .and_then(|p| p.get_mut("enabledPlugins"))
        .filter(|v| matches!(v, Ordered::Object(_)))
    else {
        return Ok(());
    };
    let mut removed = false;
    for alias in aliases(id) {
        removed |= enabled.remove(alias);
    }
    if removed {
        write(path, &tree).await?;
    }
    Ok(())
}

fn suppressed(tree: &mut Ordered) -> Vec<String> {
    match tree
        .get_mut("plugins")
        .and_then(|p| p.get_mut("suppressedBuiltins"))
    {
        Some(Ordered::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Ordered::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn set_suppressed(tree: &mut Ordered, ids: Vec<String>) {
    plugins(tree).set(
        "suppressedBuiltins",
        Ordered::Array(ids.into_iter().map(Ordered::String).collect()),
    );
}

/// Node `addSuppressedBuiltinInFileConfig` (idempotent).
pub async fn add_suppressed(path: &Path, id: &str) -> Result<()> {
    let mut tree = read(path).await?;
    let canonical_id = canonical(id).to_owned();
    let ids = aliases(&canonical_id);
    let current = suppressed(&mut tree);
    let mut retained: Vec<String> = current
        .iter()
        .filter(|s| !ids.contains(&s.as_str()))
        .cloned()
        .collect();
    if retained.len() == current.len() && current.contains(&canonical_id) {
        return Ok(());
    }
    retained.push(canonical_id);
    set_suppressed(&mut tree, retained);
    write(path, &tree).await
}

/// Node `removeSuppressedBuiltinInFileConfig`.
pub async fn remove_suppressed(path: &Path, id: &str) -> Result<()> {
    let mut tree = read(path).await?;
    let ids = aliases(id);
    let current = suppressed(&mut tree);
    let next: Vec<String> = current
        .iter()
        .filter(|s| !ids.contains(&s.as_str()))
        .cloned()
        .collect();
    if next.len() == current.len() {
        return Ok(());
    }
    set_suppressed(&mut tree, next);
    write(path, &tree).await
}

#[cfg(test)]
#[path = "config_file_tests.rs"]
mod tests;
