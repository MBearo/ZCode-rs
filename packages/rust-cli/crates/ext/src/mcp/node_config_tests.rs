//! Node 形状的 `mcp.servers`（ext.md §3.1）。
use serde_json::json;

use super::*;

#[test]
fn servers_follow_the_node_shape_and_legacy_fields() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "b-http": {"type": "remote", "url": "https://mcp.example.com/mcp", "http_headers": {"X": "${TOKEN}"}},
            "a-stdio": {"command": "node", "args": ["server.js"], "environment": {"K": "v"}, "timeout": 5},
            "c-sse": {"type": "sse", "url": "https://mcp.example.com/sse"},
            "d-off": {"command": "x", "enable": false, "enabled": true},
            "e-bad": {"type": "stdio"},
            "f-url": {"url": "https://mcp.example.com/u"}
        }),
        McpScope::User,
    );
    assert_eq!(
        warnings,
        vec!["mcp.servers.e-bad: command: required".to_owned()]
    );
    let names: Vec<_> = servers
        .iter()
        .map(|found| (found.server.name.as_str(), found.enabled))
        .collect();
    assert_eq!(
        names,
        [
            ("a-stdio", true),
            ("b-http", true),
            ("c-sse", true),
            ("d-off", false),
            ("f-url", true)
        ]
    );
    let servers: Vec<_> = servers
        .into_iter()
        .filter(|found| found.enabled)
        .map(|found| found.server)
        .collect();
    assert_eq!(
        servers[0].transport,
        McpTransportConfig::Stdio {
            command: "node".into(),
            args: vec!["server.js".into()],
            env: [("K".to_owned(), "v".to_owned())].into(),
            cwd: None,
        }
    );
    // 值原样使用，不展开 `${VAR}`（Node 不展开）。
    assert_eq!(
        servers[1].transport,
        McpTransportConfig::Http {
            url: "https://mcp.example.com/mcp".into(),
            headers: [("X".to_owned(), "${TOKEN}".to_owned())].into(),
        }
    );
    assert!(matches!(
        servers[2].transport,
        McpTransportConfig::Sse { .. }
    ));
    assert!(matches!(
        servers[3].transport,
        McpTransportConfig::Http { .. }
    ));
    assert!(servers.iter().all(|server| server.scope == McpScope::User));
}

#[test]
fn a_non_object_table_is_ignored() {
    let (servers, warnings) = parse_node_servers(&json!([1]), McpScope::Project);
    assert!(servers.is_empty());
    assert_eq!(warnings.len(), 1);
}

/// L19：`cwd`、`timeoutMs`、`alwaysLoad` 按 Node 接上；`protocolVersion`、`oauth` 未覆盖，记警告照常连接
/// （ext.md §3.1）。
#[test]
fn node_server_fields_are_read_or_reported() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "a": {"type": "stdio", "command": "node", "cwd": "tools/mcp", "timeoutMs": 45000, "alwaysLoad": true},
            "b": {"type": "http", "url": "https://mcp.example.com/mcp", "timeoutMs": 1.5,
                  "protocolVersion": "auto",
                  "oauth": {"type": "client_credentials", "clientId": "c", "clientSecret": "s"}},
            "c": {"type": "sse", "url": "https://mcp.example.com/sse", "protocolVersion": "legacy"}
        }),
        McpScope::Project,
    );
    assert_eq!(
        warnings,
        vec![
            "mcp.servers.b: protocolVersion \"auto\" is not supported; connecting with the 2025-06-18 handshake".to_owned(),
            "mcp.servers.b: oauth is not supported; connecting without it".to_owned(),
        ]
    );
    assert_eq!(
        servers[0].server.transport,
        McpTransportConfig::Stdio {
            command: "node".into(),
            args: Vec::new(),
            env: Default::default(),
            cwd: Some("tools/mcp".into()),
        }
    );
    assert_eq!(
        servers[0].server.options,
        McpServerOptions {
            timeout_ms: Some(45_000),
            always_load: true,
        }
    );
    // 小数毫秒按 Node 的正数接受，向上取整到整毫秒。
    assert_eq!(servers[1].server.options.timeout_ms, Some(2));
    assert_eq!(servers[2].server.options, McpServerOptions::default());
}

#[test]
fn bad_node_server_fields_make_the_server_invalid() {
    let (servers, warnings) = parse_node_servers(
        &json!({
            "a": {"command": "x", "cwd": ""},
            "b": {"command": "x", "timeoutMs": 0},
            "c": {"command": "x", "alwaysLoad": "yes"},
            "d": {"command": "x", "protocolVersion": "2025-06-18"},
            "e": {"url": "https://mcp.example.com", "cwd": "here"}
        }),
        McpScope::User,
    );
    assert!(servers.is_empty(), "{servers:?}");
    assert_eq!(
        warnings,
        vec![
            "mcp.servers.a: cwd: expected a non-empty string".to_owned(),
            "mcp.servers.b: timeoutMs: expected a positive number".to_owned(),
            "mcp.servers.c: alwaysLoad: expected a boolean".to_owned(),
            "mcp.servers.d: protocolVersion: expected auto, legacy or 2026-07-28".to_owned(),
            "mcp.servers.e: cwd: only stdio servers have a working directory".to_owned(),
        ]
    );
}
