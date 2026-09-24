//! Read-only plugin validation (Node `marketplace.ts` `validatePluginRoot`,
//! `validateMarketplacePlugin`, `validateMarketplaceSource`). Spec
//! rust-m10-4-plugin-sources §8.
use crate::diagnose;
use crate::manifest::{Diagnostic, Severity};
use crate::market::{Entry, Manifest};
use crate::plugin_source;
use crate::ports::Ports;
use serde_json::{Map, Value};
use std::path::Path;

const UNSUPPORTED_FIELDS: [&str; 4] = ["channels", "lspServers", "outputStyles", "settings"];
const REMOTE_KINDS: [&str; 4] = ["github", "git", "url", "git-subdir"];

fn warning(code: &'static str, message: String, plugin: &str) -> Diagnostic {
    Diagnostic::new(code, Severity::Warning, message).plugin(plugin)
}

fn bundle(value: &Value) -> bool {
    match value {
        Value::String(s) => s.ends_with(".mcpb") || s.ends_with(".dxt"),
        Value::Array(items) => items.iter().any(bundle),
        _ => false,
    }
}

/// Node `pushManifestCompatibilityDiagnostics`.
pub fn compatibility(manifest: &Value, path: &Path, plugin: &str, out: &mut Vec<Diagnostic>) {
    for key in UNSUPPORTED_FIELDS {
        if manifest.get(key).is_some() {
            let message =
                format!("Plugin component is diagnostic-only in this ZCode runtime: {key}");
            out.push(warning("plugin_unsupported_component", message, plugin).at(path));
        }
    }
    for (key, option) in manifest["userConfig"].as_object().into_iter().flatten() {
        if option["required"] == true && option.get("default").is_none() {
            let message =
                format!("Required plugin userConfig has no default and must be configured: {key}");
            out.push(warning("plugin_variable_missing", message, plugin).at(path));
        }
    }
    if bundle(&manifest["mcpServers"]) {
        let message = "MCPB/DXT plugin bundles are recognized but not supported in this runtime";
        out.push(
            warning(
                "plugin_marketplace_source_unsupported",
                message.into(),
                plugin,
            )
            .at(path),
        );
    }
}

/// Node `pushEntryCompatibilityDiagnostics`: the entry as a synthesized manifest.
fn entry_compatibility(entry: &Entry, marketplace: &str, out: &mut Vec<Diagnostic>) {
    let id = format!("{}@{marketplace}", entry.name);
    let manifest = crate::entry_manifest::synthetic(entry);
    compatibility(&manifest, Path::new(&id), &id, out);
}

/// Node `validatePluginRoot`.
pub async fn root(
    entry: &Entry,
    marketplace: &str,
    root: &Path,
    storage: &Path,
) -> Vec<Diagnostic> {
    let id = format!("{}@{marketplace}", entry.name);
    let mut out = vec![];
    let loaded = match crate::entry_manifest::read(root, entry).await {
        Err(message) => {
            let diagnostic = Diagnostic::new("plugin_manifest_invalid", Severity::Error, message);
            return vec![diagnostic.at(root).plugin(&id)];
        }
        Ok(None) => {
            let message = format!("Plugin manifest not found: {id}");
            let diagnostic = Diagnostic::new("plugin_manifest_not_found", Severity::Error, message);
            return vec![diagnostic.at(root).plugin(&id)];
        }
        Ok(Some(loaded)) => loaded,
    };
    let path = loaded.path.clone().unwrap_or_else(|| root.to_owned());
    let name = loaded.manifest["name"].as_str().unwrap_or_default();
    if name != entry.name {
        let message = format!(
            "Plugin manifest name '{name}' does not match marketplace entry '{}'",
            entry.name
        );
        let diagnostic = Diagnostic::new("plugin_manifest_invalid", Severity::Error, message);
        out.push(diagnostic.at(&path).plugin(&id));
    }
    compatibility(&loaded.manifest, &path, &id, &mut out);
    let plugin = crate::loaded::Loaded {
        id: id.clone(),
        manifest: loaded.manifest,
        manifest_path: path,
        marketplace: marketplace.to_owned(),
        root: root.to_owned(),
        source: crate::loaded::Source::Cache,
    };
    let definitions = crate::mcp::definitions(&plugin, &mut out).await;
    let context = crate::mcp::Context {
        loaded: &plugin,
        data_path: &crate::store::data_dir(storage, &id),
        cwd: &std::env::current_dir().unwrap_or_default(),
        env: &|_| None,
        options: &Map::new(),
    };
    crate::mcp::resolve(&definitions, &context, &mut out);
    out
}

/// Node `validateMarketplaceEntryShape` (without entry compatibility).
fn entry_shape(entry: &Entry, marketplace: &str, out: &mut Vec<Diagnostic>) {
    let id = format!("{}@{marketplace}", entry.name);
    let error = |message: String| {
        Diagnostic::new("plugin_marketplace_invalid", Severity::Error, message).plugin(&id)
    };
    let Some(source) = &entry.source else {
        out.push(error(format!("Plugin has no install source: {id}")));
        return;
    };
    let Some(object) = source.as_object() else {
        return;
    };
    let text = |key: &str| object.get(key).and_then(Value::as_str).unwrap_or_default();
    let kind = text("source");
    if kind == "npm" || kind == "pip" {
        let message =
            format!("Plugin source is recognized but not supported in V1 install: {kind}");
        out.push(warning(
            "plugin_marketplace_source_unsupported",
            message,
            &id,
        ));
    }
    if kind != "url" {
        return;
    }
    let kind_of_url = text("type");
    if !kind_of_url.is_empty() && kind_of_url != "git" && kind_of_url != "zip" {
        let message = format!("Plugin URL source type is not supported: {kind_of_url}");
        out.push(
            Diagnostic::new(
                "plugin_marketplace_source_unsupported",
                Severity::Error,
                message,
            )
            .plugin(&id),
        );
    }
    if let Err(message) = url_shape(object, kind_of_url == "zip") {
        out.push(error(message));
    }
}

/// Required URL, and for zip sources the sha256, headers, path and stripRoot shapes.
fn url_shape(source: &Map<String, Value>, zip: bool) -> Result<(), String> {
    match source.get("url").and_then(Value::as_str) {
        Some(url) if !crate::js::trim(url).is_empty() => {}
        _ => return Err("Plugin URL source requires a non-empty url".into()),
    }
    if !zip {
        return Ok(());
    }
    let Some(sha) = source.get("sha256").and_then(Value::as_str) else {
        return Err("Plugin zip source sha256 is required".into());
    };
    let sha = sha.to_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Plugin zip source sha256 must be a 64 character hex string".into());
    }
    match source.get("headers") {
        None => {}
        Some(Value::Object(headers)) => {
            if let Some((key, _)) = headers.iter().find(|(_, v)| !v.is_string()) {
                return Err(format!("Plugin zip source header must be a string: {key}"));
            }
        }
        Some(_) => return Err("Plugin zip source headers must be an object".into()),
    }
    if source.get("path").is_some_and(|p| !p.is_string()) {
        return Err("Plugin zip source path must be a string".into());
    }
    if source.get("stripRoot").is_some_and(|p| !p.is_boolean()) {
        return Err("Plugin zip source stripRoot must be a boolean".into());
    }
    Ok(())
}

/// Node `getMarketplaceSourceValidationDeferral`.
fn deferral(entry: &Entry, marketplace: &str) -> Option<Diagnostic> {
    let source = entry.source.as_ref()?.as_object()?;
    let text = |key: &str| source.get(key).and_then(Value::as_str);
    let kind = text("source").unwrap_or_default();
    let kind_of_url = text("type").unwrap_or_default();
    if kind == "url" && !kind_of_url.is_empty() && kind_of_url != "git" && kind_of_url != "zip" {
        return None;
    }
    if !REMOTE_KINDS.contains(&kind) {
        return None;
    }
    let label = text("repo").or_else(|| text("url")).unwrap_or(kind);
    let id = format!("{}@{marketplace}", entry.name);
    // 聚合市场可能有大量外部 git 来源；市场级校验不逐个 clone，安装或单插件校验时再深扫。
    let message = format!(
        "Remote plugin source validation is deferred until install or single-plugin validate: {label}"
    );
    Some(warning("plugin_validation_deferred", message, &id))
}

async fn dependencies(
    ports: &Ports<'_>,
    marketplace: &str,
    name: &str,
    manifest: Option<&Manifest>,
    out: &mut Vec<Diagnostic>,
) {
    let result = async {
        let allow = match manifest {
            Some(manifest) => manifest.allow_cross.clone(),
            None => crate::market::manifest(ports.storage, marketplace)
                .await?
                .map(|m| m.allow_cross)
                .unwrap_or_default(),
        };
        crate::closure::resolve(ports.storage, marketplace, name, &allow, manifest).await
    }
    .await;
    if let Err(error) = result {
        out.push(diagnose::validation(
            &error,
            Some(&format!("{name}@{marketplace}")),
        ));
    }
}

async fn resolved_root(
    input: &plugin_source::Input<'_>,
    ports: &Ports<'_>,
    out: &mut Vec<Diagnostic>,
) -> bool {
    let id = format!("{}@{}", input.entry.name, input.marketplace);
    match plugin_source::resolve(input, ports).await {
        Ok(resolved) => {
            out.extend(
                root(
                    input.entry,
                    input.marketplace,
                    &resolved.path,
                    ports.storage,
                )
                .await,
            );
            crate::store::cleanup(resolved.cleanup.as_deref()).await;
            true
        }
        Err(error) => {
            out.push(diagnose::validation(&error, Some(&id)));
            false
        }
    }
}

/// Node `validateMarketplacePlugin`.
pub async fn marketplace_plugin(
    ports: &Ports<'_>,
    marketplace: &str,
    name: &str,
) -> Vec<Diagnostic> {
    let id = format!("{name}@{marketplace}");
    let mut out = vec![];
    if let Err(error) = crate::marketplace_ops::ensure_available(ports, marketplace).await {
        return vec![diagnose::validation(&error, Some(&id))];
    }
    let path = crate::store::market_manifest(ports.storage, marketplace);
    let manifest = match crate::market::manifest(ports.storage, marketplace).await {
        Ok(Some(manifest)) => manifest,
        _ => {
            let message = format!("Marketplace not found: {marketplace}");
            let diagnostic =
                Diagnostic::new("plugin_marketplace_invalid", Severity::Error, message);
            return vec![diagnostic.at(path)];
        }
    };
    let Some(entry) = manifest.plugins.iter().find(|p| p.name == name) else {
        let message = format!("Plugin not found: {id}");
        return vec![Diagnostic::new("plugin_not_found", Severity::Error, message).at(path)];
    };
    dependencies(ports, marketplace, name, None, &mut out).await;
    let input = plugin_source::Input {
        entry,
        marketplace,
        source_root: None,
        manifest: None,
    };
    resolved_root(&input, ports, &mut out).await;
    out
}

/// Node `validateMarketplaceSource`.
pub async fn marketplace_source(
    ports: &Ports<'_>,
    source: &Value,
    expected: Option<&str>,
    plugin: Option<&str>,
) -> Vec<Diagnostic> {
    let mut out = vec![];
    let loaded = match crate::marketplace_source::load(source, ports).await {
        Ok(loaded) => loaded,
        Err(error) => return vec![diagnose::validation(&error, None)],
    };
    let manifest = &loaded.manifest;
    let name = manifest.name.as_str();
    'checks: {
        if let Some(expected) = expected.filter(|e| *e != name) {
            let message = format!(
                "Marketplace declaration id mismatch: expected {expected}, received {name}"
            );
            out.push(
                Diagnostic::new("plugin_marketplace_invalid", Severity::Error, message)
                    .plugin(expected),
            );
            break 'checks;
        }
        if manifest.plugins.is_empty() {
            let message = format!("Marketplace has no plugins: {name}");
            out.push(Diagnostic::new(
                "plugin_marketplace_invalid",
                Severity::Warning,
                message,
            ));
        }
        let entries: Vec<&Entry> = manifest
            .plugins
            .iter()
            .filter(|e| plugin.is_none_or(|p| e.name == p))
            .collect();
        if let Some(plugin) = plugin.filter(|_| entries.is_empty()) {
            let id = format!("{plugin}@{name}");
            let message = format!("Plugin not found: {id}");
            out.push(Diagnostic::new("plugin_not_found", Severity::Error, message).plugin(&id));
            break 'checks;
        }
        for entry in entries {
            entry_shape(entry, name, &mut out);
            dependencies(ports, name, &entry.name, Some(manifest), &mut out).await;
            if let Some(deferred) = deferral(entry, name) {
                out.push(deferred);
                entry_compatibility(entry, name, &mut out);
                continue;
            }
            let input = plugin_source::Input {
                entry,
                marketplace: name,
                source_root: loaded.source_root.as_deref(),
                manifest: Some(manifest),
            };
            if !resolved_root(&input, ports, &mut out).await {
                // 来源暂时无法解析时，仍基于条目原文给出仅诊断的能力风险。
                entry_compatibility(entry, name, &mut out);
            }
        }
    }
    crate::store::cleanup(loaded.cleanup.as_deref()).await;
    out
}
