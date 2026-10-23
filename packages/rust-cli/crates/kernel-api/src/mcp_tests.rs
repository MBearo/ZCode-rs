//! MCP 跨端口类型的序列化往返（README §6 规则 8）。

use std::collections::BTreeMap;

use futures_util::{FutureExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::*;
use crate::env::EnvPath;
use crate::ids::EnvironmentId;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

#[test]
fn transport_types_round_trip() {
    round_trip(&McpConnect {
        server: "files".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "node".to_owned(),
            args: vec!["server.js".to_owned()],
            env: BTreeMap::from([("TOKEN".to_owned(), "x".to_owned())]),
            cwd: Some("tools/mcp".to_owned()),
        },
    });
    round_trip(&McpTransportConfig::Http {
        url: "https://mcp.example.com/mcp".to_owned(),
        headers: BTreeMap::from([("authorization".to_owned(), "Bearer t".to_owned())]),
    });
    round_trip(&McpTransportConfig::Sse {
        url: "https://mcp.example.com/sse".to_owned(),
        headers: BTreeMap::new(),
    });
    round_trip(&McpTransportEvent::Message {
        message: json!({"jsonrpc": "2.0", "id": 0, "result": {}}),
    });
    round_trip(&McpTransportEvent::Closed {
        reason: Some("exited".to_owned()),
        session_expired: false,
    });
    round_trip(&McpError::Connect {
        message: "spawn failed".to_owned(),
    });
    round_trip(&McpToolInfo {
        server: "my server".to_owned(),
        tool: "search".to_owned(),
        always_load: true,
    });
}

#[test]
fn stdio_config_reads_the_mcp_servers_shape() {
    // `type` 缺省时是 stdio（入口层补上），这里检查显式写出的形状与配置文件的字段名一致。
    let config: McpTransportConfig =
        serde_json::from_value(json!({"type": "stdio", "command": "npx", "args": ["-y", "pkg"]}))
            .unwrap();
    assert_eq!(
        config,
        McpTransportConfig::Stdio {
            command: "npx".to_owned(),
            args: vec!["-y".to_owned(), "pkg".to_owned()],
            env: BTreeMap::new(),
            cwd: None,
        }
    );
    // L19：没有 `cwd` 时不写出（旧的序列化结果照常能读）。
    let text = serde_json::to_string(&config).unwrap();
    assert!(!text.contains("cwd"), "{text}");
}

#[test]
fn no_transport_refuses_to_connect() {
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    };
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Http {
            url: "https://mcp.example.com".to_owned(),
            headers: BTreeMap::new(),
        },
    };
    // 空实现不等待任何东西：立即完成。
    let connected = NoMcpTransport.connect(&workspace, request).now_or_never();
    assert!(matches!(connected, Some(Err(McpError::Unsupported { .. }))));
    let id = McpConnectionId::new("c1");
    assert_eq!(NoMcpTransport.events(&id).next().now_or_never(), Some(None));
}
