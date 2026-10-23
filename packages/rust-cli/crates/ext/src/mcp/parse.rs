//! MCP 服务器配置的解析与 `${VAR}` 展开（docs/specs/rust-cli/ext.md §3.1）：配置的 schema 与字符串里的变量展开。
//! `expandEnvVarsInString`）。读文件是入口层的事：它把读到的 JSON 与环境变量交给这里。

use std::collections::BTreeMap;

use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::Value;

use super::config::{McpScope, McpServer};

/// `${VAR}` 与 `${VAR:-默认}` 展开；没有值也没有默认值的变量原样保留，名字记进 `missing`。
pub fn expand_vars(
    text: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    missing: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let (name, default) = match inner.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (inner, None),
        };
        match lookup(name).or_else(|| default.map(str::to_owned)) {
            Some(value) => out.push_str(&value),
            None => {
                missing.push(name.to_owned());
                out.push_str(&rest[start..start + 2 + end + 1]);
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// 一个服务器的配置；不认识的形状返回原因。
fn transport(
    config: &Value,
    expand: &mut dyn FnMut(&str) -> String,
) -> Result<McpTransportConfig, String> {
    let kind = config
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("stdio");
    let url = || {
        config
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing url".to_owned())
    };
    match kind {
        "stdio" => {
            let command = config
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing command".to_owned())?;
            Ok(McpTransportConfig::Stdio {
                command: expand(command),
                args: strings(config.get("args"))
                    .iter()
                    .map(|arg| expand(arg))
                    .collect(),
                env: string_map(config.get("env"))
                    .into_iter()
                    .map(|(key, value)| (key, expand(&value)))
                    .collect(),
                cwd: None,
            })
        }
        "sse" | "http" => {
            let url = expand(url()?);
            let headers = string_map(config.get("headers"))
                .into_iter()
                .map(|(key, value)| (key, expand(&value)))
                .collect();
            Ok(if kind == "sse" {
                McpTransportConfig::Sse { url, headers }
            } else {
                McpTransportConfig::Http { url, headers }
            })
        }
        other => Err(format!("unsupported transport type \"{other}\"")),
    }
}

/// 服务器表（`mcpServers` 的值）里的服务器，按名字排序；返回服务器与警告。
pub fn parse_servers(
    servers: &Value,
    scope: McpScope,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (Vec<McpServer>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Some(entries) = servers.as_object() else {
        if !servers.is_null() {
            warnings.push("mcpServers must be a JSON object".to_owned());
        }
        return (found, warnings);
    };
    let mut names: Vec<&String> = entries.keys().collect();
    names.sort();
    for name in names {
        let mut missing = Vec::new();
        let parsed = {
            let mut expand = |text: &str| expand_vars(text, lookup, &mut missing);
            transport(&entries[name], &mut expand)
        };
        match parsed {
            Ok(transport) => {
                if !missing.is_empty() {
                    warnings.push(format!(
                        "MCP server \"{name}\": missing environment variables: {}",
                        missing.join(", ")
                    ));
                }
                found.push(McpServer {
                    name: name.clone(),
                    scope,
                    transport,
                    options: Default::default(),
                });
            }
            Err(reason) => warnings.push(format!("MCP server \"{name}\" skipped: {reason}")),
        }
    }
    (found, warnings)
}

/// 按作用域合并（ext.md §3.1）：enterprise 存在时只用它；否则 user < project < local，dynamic 覆盖同名。
pub fn merge_scopes(layers: Vec<Vec<McpServer>>) -> Vec<McpServer> {
    let all: Vec<McpServer> = layers.into_iter().flatten().collect();
    if all
        .iter()
        .any(|server| server.scope == McpScope::Enterprise)
    {
        return all
            .into_iter()
            .filter(|server| server.scope == McpScope::Enterprise)
            .collect();
    }
    let rank = |scope: McpScope| match scope {
        McpScope::Plugin => 0,
        McpScope::User => 1,
        McpScope::Project => 2,
        McpScope::Local => 3,
        McpScope::Dynamic | McpScope::Enterprise => 4,
    };
    let mut merged: BTreeMap<String, McpServer> = BTreeMap::new();
    for server in all {
        let keep = merged
            .get(&server.name)
            .is_none_or(|known| rank(server.scope) >= rank(known.scope));
        if keep {
            merged.insert(server.name.clone(), server);
        }
    }
    merged.into_values().collect()
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
