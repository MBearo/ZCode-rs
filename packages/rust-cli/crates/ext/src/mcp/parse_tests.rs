//! MCP 配置的解析、`${VAR}` 展开与作用域合并（ext.md §3.1）。

use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::json;

use super::*;

fn lookup(name: &str) -> Option<String> {
    (name == "TOKEN").then(|| "secret".to_owned())
}

#[test]
fn servers_parse_with_variables_expanded() {
    let table = json!({
        "web": {"type": "http", "url": "https://mcp.example.com/${PATH_PART:-mcp}", "headers": {"authorization": "Bearer ${TOKEN}"}},
        "files": {"command": "node", "args": ["server.js", "${MISSING}"], "env": {"KEY": "${TOKEN}"}},
        "bad": {"type": "ws", "url": "wss://x"},
        "nocmd": {"type": "stdio"}
    });
    let (servers, warnings) = parse_servers(&table, McpScope::Project, &lookup);
    let names: Vec<&str> = servers.iter().map(|server| server.name.as_str()).collect();
    assert_eq!(names, ["files", "web"]);
    let McpTransportConfig::Stdio { args, env, .. } = &servers[0].transport else {
        panic!("files is stdio");
    };
    assert_eq!(args, &["server.js", "${MISSING}"]);
    assert_eq!(env["KEY"], "secret");
    let McpTransportConfig::Http { url, headers } = &servers[1].transport else {
        panic!("web is http");
    };
    assert_eq!(url, "https://mcp.example.com/mcp");
    assert_eq!(headers["authorization"], "Bearer secret");
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings.iter().any(|warning| warning.contains("MISSING")));
}

fn server(name: &str, scope: McpScope, command: &str) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope,
        transport: McpTransportConfig::Stdio {
            command: command.to_owned(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
        },
        options: Default::default(),
    }
}

#[test]
fn local_beats_project_beats_user_and_enterprise_is_exclusive() {
    let merged = merge_scopes(vec![
        vec![
            server("a", McpScope::User, "user"),
            server("b", McpScope::User, "user"),
        ],
        vec![server("a", McpScope::Project, "project")],
        vec![
            server("a", McpScope::Local, "local"),
            server("c", McpScope::Local, "local"),
        ],
    ]);
    let summary: Vec<(String, McpScope)> = merged
        .iter()
        .map(|server| (server.name.clone(), server.scope))
        .collect();
    assert_eq!(
        summary,
        [
            ("a".to_owned(), McpScope::Local),
            ("b".to_owned(), McpScope::User),
            ("c".to_owned(), McpScope::Local)
        ]
    );
    let only = merge_scopes(vec![
        vec![server("x", McpScope::Enterprise, "corp")],
        vec![server("y", McpScope::User, "user")],
    ]);
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].name, "x");
}
