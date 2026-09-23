use super::{extension_config as config, extension_plugins as plugins};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct Server {
    pub invalid: bool,
    pub name: String,
    pub transport: String,
    pub raw: Value,
    pub cwd: PathBuf,
    pub timeout: std::time::Duration,
    pub workspace: bool,
    pub enabled: bool,
}
impl Server {
    pub fn parse(name: &str, mut raw: Value, cwd: &Path) -> Result<Self> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 256,
            "Invalid MCP name"
        );
        let transport = raw["type"].as_str().unwrap_or("stdio").to_owned();
        ensure!(
            ["stdio", "http", "sse"].contains(&transport.as_str()),
            "Invalid MCP transport"
        );
        for field in ["env", "headers"] {
            if let Some(entries) = raw[field].as_array() {
                let mut map = serde_json::Map::new();
                for entry in entries {
                    map.insert(
                        entry["name"]
                            .as_str()
                            .context("Invalid MCP entry name")?
                            .into(),
                        Value::String(
                            entry["value"]
                                .as_str()
                                .context("Invalid MCP entry value")?
                                .into(),
                        ),
                    );
                }
                raw[field] = Value::Object(map);
            }
            ensure!(
                raw.get(field)
                    .is_none_or(|v| v
                        .as_object()
                        .is_some_and(|o| o.iter().all(|(k, v)| !k.contains('\0')
                            && v.as_str().is_some_and(|v| !v.contains('\0'))))),
                "Invalid MCP entries"
            );
        }
        if transport == "stdio" {
            ensure!(
                raw["command"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && !s.contains('\0')),
                "Invalid MCP command"
            );
            ensure!(
                raw.get("args")
                    .is_none_or(|v| v.as_array().is_some_and(|a| a
                        .iter()
                        .all(|s| s.as_str().is_some_and(|s| !s.contains('\0'))))),
                "Invalid MCP arguments"
            );
        } else {
            let url = url::Url::parse(raw["url"].as_str().context("MCP URL required")?)
                .context("Invalid MCP URL")?;
            ensure!(
                ["http", "https"].contains(&url.scheme()),
                "MCP requires HTTP or HTTPS"
            );
        }
        let timeout = raw["timeoutMs"].as_u64().unwrap_or(30_000);
        ensure!(timeout > 0, "Invalid MCP timeout");
        ensure!(
            raw.get("isolation")
                .is_none_or(|v| v == "session" || v == "workspace"),
            "Invalid MCP isolation"
        );
        ensure!(
            raw.get("protocolVersion")
                .is_none_or(|v| v == "legacy" || v == "auto" || v == "2026-07-28"),
            "Invalid MCP protocol"
        );
        Ok(Self {
            invalid: false,
            name: name.into(),
            cwd: config::resolve(cwd, raw["cwd"].as_str().unwrap_or(".")),
            workspace: raw["isolation"] == "workspace",
            enabled: raw["enabled"] != false && raw["enable"] != false,
            raw,
            transport,
            timeout: std::time::Duration::from_millis(timeout),
        })
    }
    fn configured(name: &str, raw: Value, cwd: &Path) -> Self {
        Self::parse(name, raw.clone(), cwd).unwrap_or_else(|_| Self {
            invalid: true,
            name: name.into(),
            enabled: raw["enabled"] != false && raw["enable"] != false,
            transport: raw["type"]
                .as_str()
                .filter(|t| ["stdio", "http", "sse"].contains(t))
                .unwrap_or("stdio")
                .into(),
            raw,
            cwd: cwd.into(),
            workspace: false,
            timeout: std::time::Duration::from_secs(30),
        })
    }
    pub fn key(&self, session: &str) -> String {
        use sha2::Digest;
        format!(
            "{}:{:x}",
            if self.workspace { "workspace" } else { session },
            sha2::Sha256::digest(format!(
                "{}\0{}\0{}",
                self.name,
                self.cwd.display(),
                self.raw
            ))
        )
    }
}
pub(super) fn explicit(value: &Value, cwd: &Path) -> Result<Vec<Server>> {
    let list = value.as_array().context("MCP servers must be an array")?;
    ensure!(list.len() <= 64, "Too many MCP servers");
    let mut seen = std::collections::BTreeSet::new();
    list.iter()
        .map(|v| {
            let name = v["name"].as_str().context("MCP server name required")?;
            ensure!(seen.insert(name), "Duplicate MCP server name");
            Server::parse(name, v.clone(), cwd)
        })
        .collect()
}
pub(super) async fn configured(
    cwd: &Path,
    overrides: Option<&Value>,
    cancel: &CancellationToken,
) -> Result<Vec<Server>> {
    let config = config::load(cwd).await?;
    if config["features"]["mcp"] == false {
        return Ok(vec![]);
    }
    let mut merged = BTreeMap::new();
    for plugin in plugins::enabled(cwd, &config, cancel).await? {
        let file = config::json_file(&plugin.root.join(".mcp.json")).await?;
        let mut definitions = shape(&file).clone();
        for spec in plugin
            .manifest
            .get("mcpServers")
            .into_iter()
            .flat_map(|v| v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]))
        {
            let value = if let Some(path) = spec.as_str() {
                let path = config::resolve(&plugin.root, path);
                ensure!(
                    path.starts_with(&plugin.root),
                    "MCP manifest path escaped plugin"
                );
                config::json_file(&path).await?
            } else {
                spec
            };
            definitions.extend(shape(&value).clone());
        }
        for (name, mut raw) in definitions {
            let name = format!("plugin:{}:{name}", plugin.name);
            let root = plugin.root.to_string_lossy();
            replace_root(&mut raw, &root);
            merged.insert(name.clone(), Server::configured(&name, raw, cwd));
        }
    }
    if overrides.is_none() {
        if let Some(servers) = config["mcp"]["servers"].as_object() {
            for (name, server) in servers {
                merged.insert(name.clone(), Server::configured(name, server.clone(), cwd));
            }
        }
        // App 同时发现 .agents；冷恢复也必须读取这条持久配置来源，不能依赖 UI 再次传参。
        for base in [config::home(), cwd.to_owned()] {
            let file = config::json_file(&base.join(".agents/mcp.json")).await?;
            if let Some(servers) = file["mcpServers"].as_object() {
                for (name, server) in servers {
                    merged
                        .entry(name.clone())
                        .or_insert_with(|| Server::configured(name, server.clone(), &base));
                }
            }
        }
    }
    if let Some(overrides) = overrides {
        for server in explicit(overrides, cwd)? {
            merged.insert(server.name.clone(), server);
        }
    }
    ensure!(merged.len() <= 64, "Too many configured MCP servers");
    Ok(merged.into_values().collect())
}
fn replace_root(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => {
            *text = text
                .replace("${CLAUDE_PLUGIN_ROOT}", root)
                .replace("${ZCODE_PLUGIN_ROOT}", root)
                .replace("${pluginRoot}", root)
        }
        Value::Array(array) => array.iter_mut().for_each(|v| replace_root(v, root)),
        Value::Object(object) => object.values_mut().for_each(|v| replace_root(v, root)),
        _ => (),
    }
}
fn shape(value: &Value) -> &serde_json::Map<String, Value> {
    static EMPTY: std::sync::OnceLock<serde_json::Map<String, Value>> = std::sync::OnceLock::new();
    value["mcpServers"]
        .as_object()
        .or_else(|| value.as_object())
        .unwrap_or_else(|| EMPTY.get_or_init(Default::default))
}
pub(super) fn tool_name(server: &str, tool: &str) -> String {
    fn clean(input: &str) -> String {
        let mut output = String::new();
        for c in input.chars() {
            let c = if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            };
            if c != '_' || !output.ends_with('_') {
                output.push(c);
            }
        }
        if output.is_empty() {
            "unknown".into()
        } else {
            output
        }
    }
    format!("mcp__{}__{}", clean(server), clean(tool))
}
pub(super) fn status(server: &Server, status: &str, count: usize, failure: Option<&str>) -> Value {
    let mut value = json!({"status":status,"transport":server.transport,"toolCount":count,"updatedAt":chrono::Utc::now().to_rfc3339()});
    if let Some(kind) = failure {
        value["failureKind"] = kind.into();
        value["error"] = format!("MCP {kind}").into();
    }
    value
}
