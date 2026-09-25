//! A session's bound MCP tools: binding a server's tools and queries over them.
use super::super::mcp_config::{self, Server};
use super::super::mcp_connection::Connection;
use super::{Binding, Hub};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

impl Hub {
    fn binding<T>(&self, session: &str, name: &str, f: impl FnOnce(&Binding) -> T) -> Option<T> {
        let state = self.state.read().unwrap();
        let bindings = state.bindings.get(session)?;
        bindings.iter().find(|b| b.name == name).map(f)
    }
    /// The annotations Node records for a bound MCP tool.
    pub fn hints(&self, session: &str, name: &str) -> Option<Value> {
        self.binding(
            session,
            name,
            |b| json!({"readOnlyHint": b.read_only, "destructiveHint": b.destructive}),
        )
    }
    pub fn safe(&self, session: &str, name: &str) -> bool {
        self.binding(session, name, |b| b.safe).unwrap_or(false)
    }
}
pub(super) fn bind(
    server: &Server,
    key: &str,
    connection: &Arc<Connection>,
    names: &mut BTreeSet<String>,
) -> Result<Vec<Binding>> {
    let mut bindings = vec![];
    for tool in &connection.tools {
        let original = tool["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("MCP tool name required")?;
        let name = mcp_config::tool_name(&server.name, original);
        ensure!(names.insert(name.clone()), "MCP tool namespace collision");
        ensure!(tool["inputSchema"].is_object(), "MCP tool schema required");
        let definition = json!({"type":"function","function":{"name":name,"description":tool["description"].as_str().unwrap_or(""),"parameters":tool["inputSchema"]}});
        ensure!(
            definition.to_string().len() <= 256 * 1024,
            "MCP schema exceeds size limit"
        );
        bindings.push(Binding {
            name,
            server: server.name.clone(),
            original: original.into(),
            key: key.into(),
            safe: tool["annotations"]["readOnlyHint"] == true
                && tool["annotations"]["destructiveHint"] == false,
            read_only: tool["annotations"]["readOnlyHint"] == true,
            destructive: tool["annotations"]["destructiveHint"] == true,
            definition,
            connection: connection.clone(),
        });
    }
    Ok(bindings)
}
