//! Plugin configuration writes and uninstall (Node `bootstrap/src/plugins.ts`:
//! `setZCodePluginEnabled`, `configureZCodePlugin`, `resetZCodePluginConfig`,
//! `restoreBuiltinPlugin`, `uninstallZCodeMarketplacePlugin`). Spec
//! rust-m10-plugins §3.10.
use crate::discovery::{Outcome, Plugin};
use crate::loaded::Source;
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Where plugin configuration is written.
pub struct Paths<'a> {
    pub user: &'a Path,
    pub cwd: &'a Path,
    pub project: &'a [String],
}

impl Paths<'_> {
    /// Node `resolvePluginConfigPath`: the user file, or the current
    /// workspace's `.zcode/config.json` (never an outer project file).
    pub fn config(&self, scope: Option<&str>) -> PathBuf {
        if scope != Some("workspace") {
            return self.user.to_owned();
        }
        let workspace = self.cwd.join(".zcode").join("config.json");
        let key = comparable(&workspace);
        self.project
            .iter()
            .map(PathBuf::from)
            .find(|p| comparable(p) == key)
            .unwrap_or(workspace)
    }
}

fn comparable(path: &Path) -> String {
    let text = crate::fsx::normalize(path).to_string_lossy().into_owned();
    if cfg!(windows) {
        text.replace('\\', "/").to_lowercase()
    } else {
        text
    }
}

/// Node `resolvePluginSelector`: an exact id, else a unique manifest name.
pub fn select<'a>(plugins: &'a [Plugin], selector: &str) -> Result<&'a Plugin> {
    let selector = crate::js::trim(selector);
    if let Some(exact) = plugins.iter().find(|p| p.loaded.id == selector) {
        return Ok(exact);
    }
    let named: Vec<&Plugin> = plugins
        .iter()
        .filter(|p| p.loaded.name() == selector)
        .collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => bail!("Plugin not found: {selector}"),
        _ => bail!("Plugin name is ambiguous, use full plugin id: {selector}"),
    }
}

/// `plugins/setEnabled`: the plugin as discovered before the write, with the
/// new state and the written scope (Node `setPluginEnabled`).
pub async fn set_enabled(
    outcome: &Outcome,
    paths: &Paths<'_>,
    (selector, enabled, scope): (&str, bool, Option<&str>),
) -> Result<Value> {
    let plugin = select(&outcome.plugins, selector)?;
    crate::config_file::set_enabled(&paths.config(scope), &plugin.loaded.id, enabled).await?;
    let mut info = crate::list::info(plugin, None);
    info["enabled"] = enabled.into();
    info["enabledSource"] = scope.unwrap_or("user").into();
    Ok(json!({ "plugin": info, "enabled": enabled }))
}

/// `plugins/configure` (Node `configureZCodePlugin`); `dryRun` only validates.
pub async fn configure(outcome: &Outcome, paths: &Paths<'_>, params: &Value) -> Result<Value> {
    let selector = params["pluginId"].as_str().unwrap_or_default();
    let plugin = select(&outcome.plugins, selector)?;
    let options: Map<String, Value> = params["options"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut clear: Vec<String> = vec![];
    for key in params["clearOptionKeys"].as_array().into_iter().flatten() {
        let key = crate::js::trim(key.as_str().unwrap_or_default()).to_owned();
        if !key.is_empty() && !clear.contains(&key) {
            clear.push(key);
        }
    }
    if params["dryRun"] != true {
        let path = paths.config(params["scope"].as_str());
        crate::config_file::update_options(&path, &plugin.loaded.id, &options, &clear).await?;
    }
    Ok(json!({"pluginId":params["pluginId"],"diagnostics":[]}))
}

/// `plugins/resetConfig`: workspace drops only its enabled override; the user
/// scope drops enabled and options (Node `resetZCodePluginConfig`).
pub async fn reset(paths: &Paths<'_>, params: &Value) -> Result<Value> {
    let id = params["pluginId"].as_str().unwrap_or_default();
    let scope = params["scope"].as_str();
    let path = paths.config(scope);
    if scope == Some("workspace") {
        crate::config_file::remove_enabled(&path, id).await?;
    } else {
        crate::config_file::remove_plugin(&path, id).await?;
    }
    Ok(json!({"pluginId":id,"diagnostics":[]}))
}

/// `plugins/restoreBuiltin`: the suppression marker is dropped. Rust does not
/// seed official caches (spec §1); discovery finds the cached plugin again.
pub async fn restore_builtin(paths: &Paths<'_>, id: &str, cua_enabled: bool) -> Result<Value> {
    if id == "computer-use@zcode-plugins-official" && !cua_enabled {
        bail!("computer-use built-in plugin requires ZCODE_CUA_PRODUCT_HELPER to be enabled");
    }
    crate::config_file::remove_suppressed(paths.user, id).await?;
    Ok(json!({"pluginId":id,"diagnostics":[]}))
}

fn removed_summary(record: &crate::records::Installed) -> Value {
    let mut summary = json!({"id":record.id,"name":record.name,"marketplace":record.marketplace,
        "enabled":false,"scope":record.scope()});
    for (key, value) in [
        ("version", &record.version),
        ("installPath", &record.install_path),
        ("installedAt", &record.installed_at),
    ] {
        if !value.is_empty() {
            summary[key] = value.clone().into();
        }
    }
    summary
}

async fn remove_dir(path: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// `plugins/uninstall` (Node `uninstallZCodeMarketplacePlugin`): installed
/// records are removed (with their cache and data unless `removeCache` is
/// false); built-in plugins are suppressed, keeping their immutable cache.
pub async fn uninstall(
    outcome: &Outcome,
    (storage, paths): (&Path, &Paths<'_>),
    params: &Value,
) -> Result<Value> {
    let id = match (
        params["pluginId"].as_str(),
        params["pluginName"].as_str(),
        params["marketplace"].as_str(),
    ) {
        (Some(id), _, _) => id.to_owned(),
        (None, Some(name), Some(marketplace)) => format!("{name}@{marketplace}"),
        _ => bail!("pluginId or pluginName + marketplace is required"),
    };
    let mut records = crate::records::installed(storage).await?;
    if let Some(index) = records.iter().position(|r| r.id == id) {
        let removed = records.remove(index);
        crate::records::save_installed(storage, &records).await?;
        if params["removeCache"] != false {
            if !removed.install_path.is_empty() {
                let root = crate::fsx::normalize(&storage.join(&removed.install_path));
                remove_dir(&root).await?;
            }
            remove_dir(
                &storage
                    .join("data")
                    .join(crate::fsx::sanitize_id(&removed.id)),
            )
            .await?;
        }
        // 安装记录是 marketplace 所有权的权威证据：同时清掉可能遗留的错误抑制标记。
        crate::config_file::remove_plugin(paths.user, &removed.id).await?;
        crate::config_file::remove_suppressed(paths.user, &removed.id).await?;
        return Ok(json!({"removedPlugin":removed_summary(&removed),"diagnostics":[]}));
    }
    let builtin = outcome
        .plugins
        .iter()
        .find(|p| p.loaded.id == id && p.loaded.source == Source::Official);
    let Some(builtin) = builtin else {
        return Ok(json!({ "diagnostics": [] }));
    };
    crate::config_file::add_suppressed(paths.user, &id).await?;
    crate::config_file::remove_plugin(paths.user, &id).await?;
    remove_dir(&builtin.data_path).await?;
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let record = crate::records::Installed {
        id: id.clone(),
        name: builtin.loaded.name().to_owned(),
        marketplace: builtin.loaded.marketplace.clone(),
        version: builtin.loaded.manifest["version"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        install_path: builtin.loaded.root.to_string_lossy().into_owned(),
        installed_at: now,
        updated_at: None,
        workspace_scope: false,
        source: None,
        raw: Value::Null,
    };
    Ok(json!({"removedPlugin":removed_summary(&record),"diagnostics":[]}))
}
