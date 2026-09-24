//! `plugins/install|update|validate|describe` (Node `bootstrap/src/plugins.ts`
//! `installZCodeMarketplacePlugin`, `validateZCodePlugin`,
//! `describeZCodePlugin`; `zcode-protocol/plugins.ts` `updatePlugin`).
//! Spec rust-m10-4-plugin-sources §8.
use crate::manifest::{Diagnostic, Severity};
use crate::market_admin::{Context, repoint};
use crate::marketplace_ops::{self, Add};
use crate::official::MARKETPLACE;
use crate::records::Installed;
use anyhow::Result;
use serde_json::{Value, json};

pub enum Install {
    Done(Value),
    /// A suppressed built-in: restore it, rediscover, then call [`restored`].
    Restore(String),
}

fn empty(diagnostics: Vec<Value>) -> Value {
    json!({"dependencyClosure":[],"installedPlugins":[],"diagnostics":diagnostics})
}

fn protocol(diagnostics: &[Diagnostic]) -> Vec<Value> {
    diagnostics.iter().map(Diagnostic::protocol).collect()
}

/// Node `toInstalledPluginData(record, enabled)` projected to the protocol.
fn summary(record: &Installed, enabled: bool) -> Value {
    let mut summary = json!({"id":record.id,"name":record.name,"marketplace":record.marketplace,
        "enabled":enabled,"scope":record.scope()});
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

/// Node `materializeDeclaredMarketplaceForExplicitAction`.
async fn materialize(context: &Context<'_>, id: &str) -> Result<()> {
    let Some(source) = context.declared(id) else {
        return Ok(());
    };
    let ports = &context.ports;
    let known = crate::market::known(ports.storage).await?;
    if let Some(record) = known.iter().find(|r| r["id"] == id) {
        if record["source"] != source {
            return Err(crate::failure::Failure::Repoint(repoint(id).message).into());
        }
        if crate::market::manifest(ports.storage, id).await?.is_some() {
            return Ok(());
        }
    }
    let request = Add {
        source: &source,
        expected: Some(id),
        trusted: None,
    };
    marketplace_ops::add(ports, request).await?;
    Ok(())
}

async fn dry_run(context: &Context<'_>, marketplace: &str, name: &str) -> Result<Value> {
    let ports = &context.ports;
    let declared = context.declared(marketplace);
    let known = crate::market::known(ports.storage).await?;
    let record = known.iter().find(|r| r["id"] == marketplace);
    if let (Some(declared), Some(record)) = (&declared, record)
        && record["source"] != *declared
    {
        return Ok(empty(vec![repoint(marketplace).protocol()]));
    }
    let snapshot = crate::market::manifest(ports.storage, marketplace)
        .await?
        .is_some();
    let diagnostics = match &declared {
        Some(declared) if record.is_none() || !snapshot => {
            let expected = Some(marketplace);
            crate::validate::marketplace_source(ports, declared, expected, Some(name)).await
        }
        _ => crate::validate::marketplace_plugin(ports, marketplace, name).await,
    };
    Ok(empty(protocol(&diagnostics)))
}

/// `plugins/install` without a suppressed built-in (see [`Install::Restore`]).
pub async fn install(
    context: &Context<'_>,
    marketplace: &str,
    name: &str,
    dry: bool,
) -> Result<Install> {
    let ports = &context.ports;
    crate::market::ensure_defaults(ports.storage).await?;
    if dry {
        return Ok(Install::Done(dry_run(context, marketplace, name).await?));
    }
    let id = format!("{name}@{marketplace}");
    let bundled = crate::market::manifest(ports.storage, marketplace)
        .await?
        .and_then(|m| m.plugins.into_iter().find(|p| p.name == name))
        .and_then(|entry| entry.source)
        .is_some_and(|source| source == "filesystem" || source == "sea");
    let suppressed = crate::fsx::path_list(&context.config["plugins"]["suppressedBuiltins"]);
    if marketplace == MARKETPLACE && bundled && suppressed.contains(&id) {
        // 内置插件的 filesystem/SEA 条目只是目录指针：直接安装等同于恢复，不写安装记录。
        return Ok(Install::Restore(id));
    }
    let installed = async {
        materialize(context, marketplace).await?;
        crate::install::install(ports, marketplace, name).await
    }
    .await;
    let outcome = match installed {
        Ok(outcome) => outcome,
        Err(error) => {
            let diagnostic = crate::diagnose::install(&error, &id);
            return Ok(Install::Done(empty(vec![diagnostic.protocol()])));
        }
    };
    if marketplace == MARKETPLACE {
        for record in &outcome.records {
            crate::config_file::remove_suppressed(context.user_path, &record.id).await?;
        }
    }
    let ids: Vec<String> = outcome.records.iter().map(|r| r.id.clone()).collect();
    let fresh = crate::config_file::enable_by_default(context.user_path, &ids).await?;
    let enabled_plugins = &context.config["plugins"]["enabledPlugins"];
    let plugins: Vec<Value> = outcome
        .records
        .iter()
        .map(|record| {
            let enabled = fresh.contains(&record.id) || enabled_plugins[&record.id] == true;
            summary(record, enabled)
        })
        .collect();
    Ok(Install::Done(json!({"dependencyClosure":outcome.closure,
        "installedPlugins":plugins,"diagnostics":[]})))
}

/// The install result of a restored built-in, from discovery after the restore.
pub fn restored(id: &str, outcome: &crate::Outcome) -> Value {
    let Some(plugin) = outcome.plugins.iter().find(|p| p.loaded.id == id) else {
        let message = format!("Bundled plugin could not be restored: {id}");
        let diagnostic = Diagnostic::new("plugin_not_found", Severity::Error, message).plugin(id);
        return empty(vec![diagnostic.protocol()]);
    };
    let info = crate::list::info(plugin, None);
    let now = crate::store::now();
    let mut summary = json!({"id":id,"name":info["name"],"marketplace":info["marketplace"],
        "enabled":info["enabled"],"scope":"user","installPath":info["rootPath"],"installedAt":now,
        "componentTypes":crate::overview::plugin_types(plugin),"hookDetails":info["hookDetails"]});
    if info["description"].as_str().is_some_and(|d| !d.is_empty()) {
        summary["description"] = info["description"].clone();
    }
    if info["version"].as_str().is_some_and(|v| !v.is_empty()) {
        summary["version"] = info["version"].clone();
    }
    if info["rootPath"].as_str().is_none_or(str::is_empty) {
        summary
            .as_object_mut()
            .expect("summary")
            .remove("installPath");
    }
    json!({"dependencyClosure":[id],"installedPlugins":[summary],"diagnostics":[]})
}

/// `plugins/update`: installed records by `pluginId`, else `marketplace`, else all.
pub async fn update_targets(context: &Context<'_>, params: &Value) -> Result<Vec<Installed>> {
    let records = crate::records::installed(context.ports.storage).await?;
    Ok(records
        .into_iter()
        .filter(
            |r| match (params["pluginId"].as_str(), params["marketplace"].as_str()) {
                (Some(id), _) => r.id == id,
                (None, Some(marketplace)) => r.marketplace == marketplace,
                _ => true,
            },
        )
        .collect())
}

/// Merges per-record install results (Node `updatePlugin`).
pub fn merge(results: &[Value]) -> Value {
    let mut merged = json!({"dependencyClosure":[],"installedPlugins":[],"diagnostics":[]});
    for result in results {
        for key in ["dependencyClosure", "installedPlugins", "diagnostics"] {
            let items = result[key].as_array().cloned().unwrap_or_default();
            merged[key].as_array_mut().expect("array").extend(items);
        }
    }
    merged
}

/// `plugins/validate` (Node `validateZCodePlugin`).
pub async fn validate(context: &Context<'_>, params: &Value) -> Result<Value> {
    let ports = &context.ports;
    crate::market::ensure_defaults(ports.storage).await?;
    let diagnostics = if let Some(source) = params["source"].as_str() {
        match crate::source_input::parse(source, context.home).await {
            Ok(source) => crate::validate::marketplace_source(ports, &source, None, None).await,
            Err(error) => vec![Diagnostic::new(
                "plugin_marketplace_invalid",
                Severity::Error,
                error.to_string(),
            )],
        }
    } else if let (Some(name), Some(marketplace)) = (
        params["pluginName"].as_str(),
        params["marketplace"].as_str(),
    ) {
        match marketplace_ops::ensure_available(ports, marketplace).await {
            Ok(_) => crate::validate::marketplace_plugin(ports, marketplace, name).await,
            Err(error) => vec![
                Diagnostic::new(
                    "plugin_marketplace_invalid",
                    Severity::Error,
                    error.to_string(),
                )
                .plugin(&format!("{name}@{marketplace}")),
            ],
        }
    } else {
        vec![]
    };
    let ok = diagnostics.iter().all(|d| d.severity != Severity::Error);
    Ok(
        json!({"ok":ok,"diagnostics":protocol(&diagnostics),"compatibility":{
        "runnable":["skills","commands","hooks","mcpServers","userConfig"],
        "diagnosticOnly":["agents","lspServers","outputStyles","channels","settings"],
        "unsupported":["mcpb","dxt","npm","hostPattern","pathPattern"]}}),
    )
}

/// `plugins/describe` (Node `describeZCodePlugin`).
pub async fn describe(context: &Context<'_>, params: &Value) -> Result<Value> {
    let ports = &context.ports;
    crate::market::ensure_defaults(ports.storage).await?;
    let marketplace = params["marketplace"].as_str().unwrap_or_default();
    let name = params["pluginName"].as_str().unwrap_or_default();
    let described = crate::describe::describe(ports, marketplace, name).await;
    Ok(crate::describe::protocol(&described))
}
