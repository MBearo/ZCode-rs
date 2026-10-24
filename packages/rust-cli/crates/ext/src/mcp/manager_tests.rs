//! 连接管理：握手、工具快照、调用、instructions、list_changed、失败与重连（ext.md §3.3–§3.6）。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::MemoryEnvironment;
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::mcp::McpScope;
use crate::test_support::{FakeMcp, facts, server, tool_context, workspace};

fn config(name: &str, remote: bool) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::Project,
        transport: if remote {
            McpTransportConfig::Http {
                url: "https://mcp.example.com/mcp".to_owned(),
                headers: Default::default(),
            }
        } else {
            McpTransportConfig::Stdio {
                command: "node".to_owned(),
                args: Vec::new(),
                env: Default::default(),
                cwd: None,
            }
        },
        options: Default::default(),
    }
}

fn files_server() -> Arc<crate::test_support::Handler> {
    server(
        json!({"tools": {}, "resources": {}}),
        Some("Use the files tools for files."),
        |method, params| match method {
            "tools/list" => Some(Ok(json!({"tools": [
                {"name": "read", "description": "Read a file", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}, "annotations": {"readOnlyHint": true}},
                {"name": "fail", "inputSchema": {"type": "object"}, "_meta": {"zcode/alwaysLoad": true}}
            ]}))),
            "tools/call" if params["name"] == "read" => Some(Ok(
                json!({"content": [{"type": "text", "text": format!("read {}", params["arguments"]["path"].as_str().unwrap_or_default())}]}),
            )),
            "tools/call" => Some(Ok(
                json!({"isError": true, "content": [{"type": "text", "text": "it failed"}]}),
            )),
            "resources/list" => Some(Ok(
                json!({"resources": [{"uri": "file:///a", "name": "a"}]}),
            )),
            "resources/read" => Some(Ok(
                json!({"contents": [{"uri": "file:///a", "mimeType": "text/plain", "text": "A"}]}),
            )),
            _ => None,
        },
    )
}

fn manager(fake: Arc<FakeMcp>, scope: &TaskScope) -> McpManager {
    McpManager::new(
        fake,
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    )
}

fn text(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text { text } => text.clone(),
            ToolResultContent::Image { .. } => "<image>".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn connects_lists_and_calls_tools() {
    let fake = Arc::new(
        FakeMcp::default()
            .with("files", files_server())
            .refusing("broken"),
    );
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake.clone(), &scope);
    manager.start(
        workspace(),
        vec![config("files", false), config("broken", false)],
    );
    manager.wait_ready().await;
    assert_eq!(
        manager.status(),
        [
            (
                "broken".to_owned(),
                McpStatus::Failed {
                    reason: "spawn failed".to_owned()
                }
            ),
            ("files".to_owned(), McpStatus::Connected),
        ]
    );
    // `/mcp` 的快照（tui-display.md §3.4）：失败原因与已连接服务器的工具数。
    let infos = manager.server_infos();
    assert_eq!(
        infos[0].state,
        rust_cli_kernel_api::manage::McpServerState::Failed
    );
    assert_eq!(infos[0].reason.as_deref(), Some("spawn failed"));
    assert_eq!(
        infos[1].state,
        rust_cli_kernel_api::manage::McpServerState::Connected
    );
    // 服务器自己的两个工具；三个资源工具是公共的，不算在某个服务器上。
    assert_eq!(infos[1].tools, 2);
    let names: Vec<String> = manager
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "ListMcpResourcesTool",
            "ReadMcpResourceDirTool",
            "ReadMcpResourceTool",
            "mcp__files__fail",
            "mcp__files__read"
        ]
    );
    let read = manager.find("mcp__files__read").unwrap();
    assert!(read.is_read_only(&json!({})));
    assert_eq!(read.mcp_info().unwrap().tool, "read");
    assert!(
        manager
            .find("mcp__files__fail")
            .unwrap()
            .mcp_info()
            .unwrap()
            .always_load
    );
    // 握手：initialize 之后发 initialized。
    let methods: Vec<String> = fake
        .received()
        .iter()
        .filter_map(|(_, message)| message["method"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        methods[..3],
        ["initialize", "notifications/initialized", "tools/list"]
    );
    let context = tool_context(
        Arc::new(MemoryEnvironment::new(facts())),
        PermissionMode::Default,
    );
    let (progress, _ignored) = mpsc::channel(4);
    let output = read
        .call(json!({"path": "a.txt"}), context.clone(), progress.clone())
        .await
        .unwrap();
    assert_eq!(text(&output), "read a.txt");
    let call = fake
        .received()
        .into_iter()
        .find(|(_, message)| message["method"] == "tools/call")
        .unwrap();
    // `_meta` 只带本工程的键：tool_use 的 ID（ext.md §3.3、D35）。
    assert_eq!(
        call.1["params"]["_meta"],
        json!({"zcode/toolUseId": "toolu_1"})
    );
    // 服务器报告的错误：原文、标记为错误。
    let failed = manager
        .find("mcp__files__fail")
        .unwrap()
        .call(json!({}), context.clone(), progress.clone())
        .await
        .unwrap();
    assert!(failed.is_error);
    assert_eq!(text(&failed), "it failed");
    // 资源对象的键按 SDK schema 的顺序（M1：name 在 uri 前）。
    let listed = manager
        .find("ListMcpResourcesTool")
        .unwrap()
        .call(json!({}), context.clone(), progress.clone())
        .await
        .unwrap();
    assert_eq!(
        text(&listed),
        r#"[{"name":"a","uri":"file:///a","server":"files"}]"#
    );
    let resource = manager
        .find("ReadMcpResourceTool")
        .unwrap()
        .call(
            json!({"server": "files", "uri": "file:///a"}),
            context,
            progress,
        )
        .await
        .unwrap();
    assert_eq!(
        text(&resource),
        r#"{"contents":[{"uri":"file:///a","mimeType":"text/plain","text":"A"}]}"#
    );
    assert_eq!(
        manager.instructions(),
        [ListingEntry {
            name: "files".to_owned(),
            line: "## files\nUse the files tools for files.".to_owned(),
        }]
    );
}

#[tokio::test]
async fn list_changed_refreshes_and_stdio_closing_fails() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    let fake = Arc::new(FakeMcp::default().with(
        "dyn",
        server(
            json!({"tools": {"listChanged": true}}),
            None,
            move |method, _| match method {
                "tools/list" => {
                    let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let tools: Vec<_> = (0..=n)
                        .map(
                            |i| json!({"name": format!("t{i}"), "inputSchema": {"type": "object"}}),
                        )
                        .collect();
                    Some(Ok(json!({"tools": tools})))
                }
                _ => None,
            },
        ),
    ));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake.clone(), &scope);
    manager.start(workspace(), vec![config("dyn", false)]);
    manager.wait_ready().await;
    assert_eq!(manager.tools().len(), 1);
    let mut changes = manager.0.changed.subscribe();
    fake.push(
        "dyn",
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
    );
    while manager.tools().len() != 2 {
        changes.changed().await.unwrap();
    }
    fake.drop_server("dyn", false);
    while manager.status()[0].1 == McpStatus::Connected
        || manager.status()[0].1 == McpStatus::Pending
    {
        changes.changed().await.unwrap();
    }
    assert!(matches!(&manager.status()[0].1, McpStatus::Failed { reason } if reason == "gone"));
    assert!(manager.tools().is_empty());
}

#[tokio::test]
async fn remote_servers_reconnect_after_closing() {
    let fake = Arc::new(FakeMcp::default().with("web", files_server()));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake.clone(), &scope);
    manager.start(workspace(), vec![config("web", true)]);
    manager.wait_ready().await;
    let first = manager.0.client("web").unwrap();
    fake.drop_server("web", true);
    let mut changes = manager.0.changed.subscribe();
    loop {
        if let Some(client) = manager.0.client("web")
            && !Arc::ptr_eq(&client, &first)
        {
            break;
        }
        changes.changed().await.unwrap();
    }
    assert_eq!(manager.status()[0].1, McpStatus::Connected);
    let initializes = fake
        .received()
        .iter()
        .filter(|(_, message)| message["method"] == "initialize")
        .count();
    assert_eq!(initializes, 2);
}

/// 延迟加载（ext.md §3.4、§3.7，tools-more.md §3.1）：MCP 工具除 `alwaysLoad`（工具的 `_meta` 标记，ext.md
/// D35，两个键见 `tool_tests.rs`）外都延迟，资源工具按 `shouldDefer` 延迟并带 `searchHint` 与旧名。
#[tokio::test]
async fn mcp_tools_are_deferred_unless_always_load() {
    use rust_cli_kernel_api::tool_search::is_deferred;
    let fake = Arc::new(FakeMcp::default().with("files", files_server()));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake, &scope);
    manager.start(workspace(), vec![config("files", false)]);
    manager.wait_ready().await;
    let deferred = |name: &str| is_deferred(manager.find(name).unwrap().as_ref());
    assert!(manager.find("mcp__files__read").unwrap().is_mcp());
    assert!(deferred("mcp__files__read"));
    assert!(manager.find("mcp__files__fail").unwrap().always_load());
    assert!(!deferred("mcp__files__fail"));
    for (name, alias, hint) in [
        (
            "ListMcpResourcesTool",
            "ListMcpResources",
            "list resources from connected MCP servers",
        ),
        (
            "ReadMcpResourceTool",
            "ReadMcpResource",
            "read a specific MCP resource by URI",
        ),
        (
            "ReadMcpResourceDirTool",
            "ReadMcpResourceDir",
            "list the children of an MCP directory resource",
        ),
    ] {
        let tool = manager.find(name).unwrap();
        assert!(!tool.is_mcp(), "{name}");
        assert!(deferred(name), "{name}");
        assert_eq!(tool.search_hint(), Some(hint));
        assert_eq!(tool.aliases(), [alias]);
    }
}

/// Node 配置的按服务器选项（ext.md §3.1）：`timeoutMs` 替换这个服务器的连接与工具调用期限，`alwaysLoad`
/// 让这个服务器的工具都不延迟；别的服务器仍用缺省值。
#[tokio::test]
async fn node_options_apply_per_server() {
    let silent = server(json!({}), None, |_, _| None);
    let silent: Arc<crate::test_support::Handler> = Arc::new(
        move |method: &str, params: &serde_json::Value| match method {
            // 不回 initialize：连接超时。
            "initialize" => None,
            other => silent(other, params),
        },
    );
    let hanging = server(json!({"tools": {}}), None, |method, _| match method {
        "tools/list" => Some(Ok(
            json!({"tools": [{"name": "wait", "inputSchema": {"type": "object"}}]}),
        )),
        // 不回 tools/call：调用超时。
        _ => None,
    });
    let fake = Arc::new(
        FakeMcp::default()
            .with("silent", silent)
            .with("hanging", hanging)
            .with("files", files_server()),
    );
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(fake, &scope);
    let with = |name: &str, timeout_ms: Option<u64>, always_load: bool| McpServer {
        options: McpServerOptions {
            timeout_ms,
            always_load,
        },
        ..config(name, false)
    };
    manager.start(
        workspace(),
        vec![
            with("silent", Some(50), false),
            with("hanging", Some(1_000), false),
            with("files", None, true),
        ],
    );
    manager.wait_ready().await;
    let status = manager.status();
    assert!(
        status.contains(&(
            "silent".to_owned(),
            McpStatus::Failed {
                reason: "MCP server \"silent\" connection timed out after 50ms".to_owned()
            }
        )),
        "{status:?}"
    );
    // `alwaysLoad`：没有 `_meta` 的工具也不延迟。
    let read = manager.find("mcp__files__read").unwrap();
    assert!(read.mcp_info().unwrap().always_load);
    let context = tool_context(
        Arc::new(MemoryEnvironment::new(facts())),
        PermissionMode::Default,
    );
    let (progress, _ignored) = mpsc::channel(4);
    let error = manager
        .find("mcp__hanging__wait")
        .unwrap()
        .call(json!({}), context, progress)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "MCP server \"hanging\" tool \"wait\" timed out after 1s"
    );
}

/// 握手上报的客户端版本来自入口（docs/specs/rust-cli/version.md §4），ext 不读自己的包版本。
#[tokio::test]
async fn the_handshake_reports_the_client_version_from_the_entry() {
    let fake = Arc::new(FakeMcp::default().with("files", files_server()));
    let scope = TaskScope::root("test", |_| {});
    let settings = McpSettings {
        client_version: "0.1.42".to_owned(),
        ..McpSettings::default()
    };
    let manager = McpManager::new(
        fake.clone(),
        scope.spawner(),
        settings,
        crate::clock_tests::TokioClock::new(),
    );
    manager.start(workspace(), vec![config("files", false)]);
    manager.wait_ready().await;
    let received = fake.received();
    let (_, initialize) = received
        .iter()
        .find(|(_, message)| message["method"] == "initialize")
        .unwrap();
    assert_eq!(
        initialize["params"]["clientInfo"],
        json!({"name": "zcode", "version": "0.1.42"})
    );
}

/// 缺陷 R2-14：失败原因原来整段按 warn 记日志，HTTP 错误里最多带 64 KiB 的响应体。日志只记前 300 个字符
/// （与 `tool.failed` 相同，logging.md §4）；界面上的原因不截断。
#[test]
fn the_logged_failure_reason_is_cut_to_300_characters() {
    let long = format!("HTTP 500: {}", "é".repeat(10_000));
    let logged = logged_reason(&long);
    assert_eq!(logged.chars().count(), 300);
    assert!(long.starts_with(&logged));
    assert_eq!(logged_reason("closed"), "closed");
}
