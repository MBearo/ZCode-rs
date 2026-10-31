//! MCP 服务器配置的解析与 `${VAR}` 展开（docs/specs/rust-cli/ext.md §3.1）：配置的 schema 与字符串里的变量展开。
//! `expandEnvVarsInString`）。读文件是入口层的事：它把读到的 JSON 与环境变量交给这里。

use std::collections::BTreeMap;

use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::Value;

use super::config::{McpScope, McpServer};

/// `${VAR}` 与 `${VAR:-默认}` 展开：规则只有一份，在 kernel-api（远端的执行器也用它，remote-exec.md §22.7）。
pub use rust_cli_kernel_api::mcp_vars::expand_vars;

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
    parse_servers_keeping(servers, scope, lookup, &|_| false)
}

/// 同 [`parse_servers`]；`keep(名字)` 为真的服务器按原文、不展开 `${VAR}`（绑定到远端机器的，本机的值不进请求）。
pub fn parse_servers_keeping(
    servers: &Value,
    scope: McpScope,
    lookup: &dyn Fn(&str) -> Option<String>,
    keep: &dyn Fn(&str) -> bool,
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
            let mut expand = |text: &str| match keep(name) {
                // 绑定到远端机器的服务器（R5，remote-exec.md §22.7）：按原文，`${VAR}` 留给那台机器的执行器展开。
                true => text.to_owned(),
                false => expand_vars(text, lookup, &mut missing),
            };
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
                    machine: None,
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
    // 同名时后者覆盖前者：插件 < 用户 < 项目 < 本地 < 动态与企业。
    const PLUGIN_RANK: u8 = 0;
    const USER_RANK: u8 = 1;
    const PROJECT_RANK: u8 = 2;
    const LOCAL_RANK: u8 = 3;
    const DYNAMIC_RANK: u8 = 4;
    let rank = |scope: McpScope| match scope {
        McpScope::Plugin => PLUGIN_RANK,
        McpScope::User => USER_RANK,
        McpScope::Project => PROJECT_RANK,
        McpScope::Local => LOCAL_RANK,
        McpScope::Dynamic | McpScope::Enterprise => DYNAMIC_RANK,
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
