//! MCP 服务器绑定到远端机器（docs/specs/rust-cli/remote-exec.md §22.2）：rust-cli 自己的旁路文件
//! `~/.zcode/rust-cli/mcp-machines.json`。
//!
//! 为什么不写进共用的配置文件：App 设置页编辑一个服务器时按表单重新拼出整个服务器（丢掉 `machine`），Node CLI 的
//! strict 校验对多出的字段报 `config_mcp_server_invalid` 并跳过这个服务器（§22.2 的核对）。只读，用户手写。

use std::collections::BTreeMap;

use rust_cli_ext::mcp::McpServer;
use rust_cli_kernel_api::env::{EnvError, EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_registry::LOCAL_MACHINE;
use rust_cli_kernel_api::machine::Files;
use serde_json::Value;

/// 服务器名 → 远端机器名（`local` 与空串不记：就是本机）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct McpBindings(BTreeMap<String, String>);

impl McpBindings {
    /// 解析文件内容；返回绑定与警告（不是 JSON、形状不对时整份为空；坏的项跳过）。
    pub(crate) fn parse(bytes: &[u8]) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let value: Value = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!("not valid JSON: {error}"));
                return (Self::default(), warnings);
            }
        };
        let Some(servers) = value.get("servers").and_then(Value::as_object) else {
            warnings.push("\"servers\" must be a JSON object".to_owned());
            return (Self::default(), warnings);
        };
        let mut bindings = BTreeMap::new();
        for (name, entry) in servers {
            match entry.get("machine").and_then(Value::as_str) {
                Some(machine) if machine.is_empty() || machine == LOCAL_MACHINE => {}
                Some(machine) => {
                    bindings.insert(name.clone(), machine.to_owned());
                }
                None => warnings.push(format!(
                    "servers.{name}: expected an object with a string \"machine\""
                )),
            }
        }
        (Self(bindings), warnings)
    }

    /// 读旁路文件（经本机的文件端口）；不存在时没有绑定，读不出或格式不对时记 warn、按没有绑定。
    pub(crate) async fn load(files: &dyn Files, workspace: &WorkspaceRef, path: &str) -> Self {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let bytes = match files.read_file(workspace, request).await {
            Ok(content) => content.bytes,
            Err(EnvError::NotFound { .. }) => return Self::default(),
            Err(error) => {
                tracing::warn!(path, %error, "the MCP machine bindings could not be read");
                return Self::default();
            }
        };
        let (bindings, warnings) = Self::parse(&bytes);
        for warning in warnings {
            tracing::warn!(path, warning, "MCP machine bindings");
        }
        bindings
    }

    /// 这个服务器绑定的远端机器。
    pub(crate) fn machine(&self, server: &str) -> Option<&str> {
        self.0.get(server).map(String::as_str)
    }

    /// 给合并之后的服务器设上绑定（插件的服务器不经这里，§22.2）。
    pub(crate) fn bind(&self, servers: &mut [McpServer]) {
        for server in servers {
            server.machine = self.machine(&server.name).map(str::to_owned);
        }
    }
}

#[cfg(test)]
#[path = "mcp_machines_tests.rs"]
mod tests;
