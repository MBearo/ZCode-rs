//! Node CLI 配置文件里的 `mcp.servers`（docs/specs/rust-cli/ext.md §3.1、config.md §2）。
//!
//! 外形是 Node 的 `McpServerConfig`（`apps/zcode-cli/packages/adapters/src/config/schema.ts`），解析前先做
//! Node 的旧格式规范化（`normalizeMcpServerConfigInput`）。值原样使用，不展开 `${VAR}`（Node 不展开）。
//! `cwd`、`timeoutMs`、`alwaysLoad` 按 Node 接上；`protocolVersion`、`oauth` 未覆盖，记警告照常连接（ext.md §3.1、
//! D21）。

use std::collections::BTreeMap;

use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::{Map, Value};

use super::config::{McpScope, McpServer, McpServerOptions};
use super::connection::PROTOCOL_VERSION;

/// Node 配置里 `protocolVersion` 认的较新版本（不支持，按 [`PROTOCOL_VERSION`] 握手；F55：以前写了两次）。
const NEWER_PROTOCOL: &str = "2026-07-28";

/// Node `normalizeMcpServerConfigInput`：旧字段名与缺省的 `type`。
fn normalize(server: &Map<String, Value>) -> Map<String, Value> {
    let mut server = server.clone();
    if !server.contains_key("env")
        && let Some(environment) = server.get("environment").cloned()
    {
        server.insert("env".into(), environment);
    }
    server.remove("environment");
    let enable = server.get("enable").and_then(Value::as_bool);
    let enabled = server.get("enabled").and_then(Value::as_bool);
    if enable.is_some() || enabled.is_some() {
        // 冲突时以停用为准（Node 注释：桌面端写 enable:false 时不清理外部导入的 enabled:true）。
        let value = if enable == Some(false) {
            false
        } else {
            enabled.unwrap_or(true)
        };
        server.insert("enabled".into(), Value::Bool(value));
    }
    server.remove("enable");
    server.remove("timeout");
    server.remove("startup_timeout_sec");
    let kind = server
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match kind.as_deref() {
        Some("remote") => {
            server.insert("type".into(), "http".into());
        }
        Some(_) => {}
        None => {
            let present = |key: &str| {
                server
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
            };
            if present("command") {
                server.insert("type".into(), "stdio".into());
            } else if present("url") {
                server.insert("type".into(), "http".into());
            }
        }
    }
    let remote = matches!(
        server.get("type").and_then(Value::as_str),
        Some("http" | "sse")
    );
    if remote
        && !server.contains_key("headers")
        && let Some(headers) = server.get("http_headers").cloned()
    {
        server.insert("headers".into(), headers);
    }
    server.remove("http_headers");
    server
}

fn string_map(value: Option<&Value>, key: &str) -> Result<BTreeMap<String, String>, String> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(fields)) => fields
            .iter()
            .map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_owned()))
                    .ok_or_else(|| format!("{key}.{name}: expected a string"))
            })
            .collect(),
        Some(_) => Err(format!("{key}: expected an object")),
    }
}

fn required(server: &Map<String, Value>, key: &str) -> Result<String, String> {
    server
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{key}: required"))
}

/// 配置文件里的一个服务器。停用的（`enabled: false`）也要给出：合并时它照样遮住低优先级的同名服务器，
/// 合并之后才去掉（Node 按名覆盖后再按 `enabled` 跳过连接）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeMcpServer {
    pub server: McpServer,
    pub enabled: bool,
}

/// 解析出的一个服务器：传输、是否启用、按服务器的选项与不影响连接的警告。
struct Parsed {
    transport: McpTransportConfig,
    enabled: bool,
    options: McpServerOptions,
    warnings: Vec<String>,
}

/// Node `mcpServerBaseSchema` 的其余字段（ext.md §3.1）：类型不对时整个服务器不合法（Node 的 strict 校验）。
fn extra_fields(
    server: &Map<String, Value>,
    stdio: bool,
) -> Result<(Option<String>, McpServerOptions, Vec<String>), String> {
    let cwd = match server.get("cwd") {
        None => None,
        Some(_) if !stdio => return Err("cwd: only stdio servers have a working directory".into()),
        Some(value) => Some(
            value
                .as_str()
                .filter(|cwd| !cwd.is_empty())
                .ok_or("cwd: expected a non-empty string")?
                .to_owned(),
        ),
    };
    let timeout_ms = match server.get("timeoutMs") {
        None => None,
        Some(value) => {
            let millis = value
                .as_f64()
                .filter(|millis| millis.is_finite() && *millis > 0.0)
                .ok_or("timeoutMs: expected a positive number")?;
            // 期限按整毫秒：小数向上取整。
            Some(millis.ceil() as u64)
        }
    };
    let always_load = match server.get("alwaysLoad") {
        None => false,
        Some(value) => value.as_bool().ok_or("alwaysLoad: expected a boolean")?,
    };
    let mut warnings = Vec::new();
    match server.get("protocolVersion").map(Value::as_str) {
        None | Some(Some("legacy")) => {}
        Some(Some(version @ ("auto" | NEWER_PROTOCOL))) => warnings.push(format!(
            "protocolVersion \"{version}\" is not supported; connecting with the {PROTOCOL_VERSION} handshake"
        )),
        Some(_) => {
            return Err(format!(
                "protocolVersion: expected auto, legacy or {NEWER_PROTOCOL}"
            ));
        }
    }
    if server.contains_key("oauth") {
        warnings.push("oauth is not supported; connecting without it".to_owned());
    }
    let options = McpServerOptions {
        timeout_ms,
        always_load,
    };
    Ok((cwd, options, warnings))
}

/// 一个服务器；`Err` 是不合法。
fn server(value: &Value) -> Result<Parsed, String> {
    let server = normalize(value.as_object().ok_or("<server>: expected an object")?);
    let enabled = server.get("enabled") != Some(&Value::Bool(false));
    let stdio = server.get("type").and_then(Value::as_str) == Some("stdio");
    if stdio {
        required(&server, "command")?;
    }
    let (cwd, options, warnings) = extra_fields(&server, stdio)?;
    let transport = match server.get("type").and_then(Value::as_str) {
        Some("stdio") => McpTransportConfig::Stdio {
            command: required(&server, "command")?,
            args: match server.get("args") {
                None => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<_>>()
                    .ok_or("args: expected an array of strings")?,
                Some(_) => return Err("args: expected an array of strings".to_owned()),
            },
            env: string_map(server.get("env"), "env")?,
            cwd,
        },
        Some(kind @ ("http" | "sse")) => {
            let url = required(&server, "url")?;
            let headers = string_map(server.get("headers"), "headers")?;
            if kind == "sse" {
                McpTransportConfig::Sse { url, headers }
            } else {
                McpTransportConfig::Http { url, headers }
            }
        }
        Some(other) => return Err(format!("type: unsupported transport \"{other}\"")),
        None => return Err("type: required".to_owned()),
    };
    Ok(Parsed {
        transport,
        enabled,
        options,
        warnings,
    })
}

/// `mcp.servers` 的值里的服务器（按名字排序），返回服务器与警告（Node `config_mcp_server_invalid`）。
pub fn parse_node_servers(servers: &Value, scope: McpScope) -> (Vec<NodeMcpServer>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Some(entries) = servers.as_object() else {
        warnings.push("mcp.servers must be a JSON object; ignoring all MCP servers.".to_owned());
        return (found, warnings);
    };
    let mut names: Vec<&String> = entries.keys().collect();
    names.sort();
    for name in names {
        match server(&entries[name]) {
            Ok(parsed) => {
                let prefix = |warning: String| format!("mcp.servers.{name}: {warning}");
                warnings.extend(parsed.warnings.into_iter().map(prefix));
                found.push(NodeMcpServer {
                    server: McpServer {
                        name: name.clone(),
                        scope,
                        transport: parsed.transport,
                        options: parsed.options,
                        machine: None,
                    },
                    enabled: parsed.enabled,
                });
            }
            Err(reason) => warnings.push(format!("mcp.servers.{name}: {reason}")),
        }
    }
    (found, warnings)
}

#[cfg(test)]
#[path = "node_config_tests.rs"]
mod tests;
