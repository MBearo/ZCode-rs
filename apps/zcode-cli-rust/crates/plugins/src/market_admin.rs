//! `plugins/marketplace/add|Remove|Update` (Node `bootstrap/src/plugins.ts`
//! `addZCodePluginMarketplace`, `removeZCodePluginMarketplace`,
//! `updateZCodePluginMarketplace`). Spec rust-m10-4-plugin-sources §7.
use crate::manifest::{Diagnostic, Severity};
use crate::marketplace_ops::{self, Add};
use crate::ports::Ports;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;

/// Configuration inputs of the management methods.
pub struct Context<'a> {
    pub ports: Ports<'a>,
    /// The merged configuration view.
    pub config: &'a Value,
    pub user_path: &'a Path,
    pub home: &'a str,
}

impl Context<'_> {
    pub fn declared(&self, id: &str) -> Option<Value> {
        crate::market::declared(self.config, self.user_path)
            .into_iter()
            .rev()
            .find(|(declared, _)| declared == id)
            .map(|(_, source)| source)
    }
}

/// Node `createMarketplaceSourceRepointDiagnostic`.
pub fn repoint(id: &str) -> Diagnostic {
    let message = format!(
        "Workspace marketplace declaration \"{id}\" conflicts with an existing Host source. Remove the existing marketplace or use a different marketplace id before materializing it."
    );
    Diagnostic::new("plugin_marketplace_invalid", Severity::Error, message).plugin(id)
}

/// `plugins/marketplace/add`; `dryRun` only parses the source.
pub async fn add(context: &Context<'_>, params: &Value) -> Result<Value> {
    let source =
        crate::source_input::parse(params["source"].as_str().unwrap_or_default(), context.home)
            .await?;
    if params["dryRun"] == true {
        let summary = json!({"id":"dry-run","name":"dry-run","source":source,"pluginCount":0,"isOfficial":false});
        return Ok(json!({"marketplace":summary,"diagnostics":[]}));
    }
    let request = Add {
        source: &source,
        expected: None,
        trusted: None,
    };
    let record = marketplace_ops::add(&context.ports, request).await?;
    Ok(json!({"marketplace":crate::known::summary(&record),"diagnostics":[]}))
}

/// `plugins/marketplace/remove`.
pub async fn remove(context: &Context<'_>, params: &Value) -> Result<Value> {
    let id = params["marketplace"].as_str().unwrap_or_default();
    crate::known::remove(context.ports.storage, id).await?;
    Ok(json!({ "diagnostics": [] }))
}

/// `plugins/marketplace/update` (Node `updateZCodePluginMarketplace`): only
/// known records refresh in bulk; a declaration is materialized when named.
pub async fn update(context: &Context<'_>, params: &Value) -> Result<Value> {
    let ports = &context.ports;
    crate::market::ensure_defaults(ports.storage).await?;
    let known = crate::market::known(ports.storage).await?;
    let known_source = |id: &str| {
        known
            .iter()
            .rev()
            .find(|r| r["id"] == id)
            .map(|r| r["source"].clone())
    };
    let named = params["marketplace"].as_str();
    let targets: Vec<String> = match named {
        Some(id) => vec![id.to_owned()],
        None => {
            let mut ids: Vec<String> = vec![];
            for id in known.iter().filter_map(|r| r["id"].as_str()) {
                if !ids.iter().any(|i| i == id) {
                    ids.push(id.to_owned());
                }
            }
            ids
        }
    };
    if let Some(id) = named
        && known_source(id).is_none()
        && context.declared(id).is_none()
    {
        bail!("Marketplace not found: {id}");
    }
    let mut updated = vec![];
    let mut diagnostics = vec![];
    for id in &targets {
        let declared = context.declared(id);
        let current = known_source(id);
        match (&declared, &current) {
            (Some(declared), Some(current)) if named.is_some() && declared != current => {
                diagnostics.push(repoint(id).protocol());
            }
            (Some(declared), None) => {
                let request = Add {
                    source: declared,
                    expected: Some(id),
                    trusted: None,
                };
                match marketplace_ops::add(ports, request).await {
                    Ok(record) => updated.push(record),
                    Err(error) => diagnostics.push(crate::diagnose::refresh(&error, id).protocol()),
                }
            }
            _ => updated.extend(marketplace_ops::refresh(ports, id).await?),
        }
    }
    let records = crate::market::known(ports.storage).await?;
    for record in &records {
        let id = record["id"].as_str().unwrap_or_default();
        if named.is_some_and(|n| n != id) {
            continue;
        }
        // 持久化的失败码可能由 Node 写入，原样投影。
        if let Some(failure) = record.get("lastRefreshFailure").filter(|f| f.is_object()) {
            diagnostics.push(json!({"code":failure["code"],"message":failure["message"],
                "pluginId":id,"severity":"error"}));
        }
    }
    let summaries: Vec<Value> = updated.iter().map(crate::known::summary).collect();
    Ok(json!({"marketplaces":summaries,"diagnostics":diagnostics}))
}
