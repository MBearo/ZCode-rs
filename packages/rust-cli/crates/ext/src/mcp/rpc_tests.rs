//! JSON-RPC 客户端：请求配对、服务器请求的回答、通知、关闭（ext.md §3.3）。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::mcp::{McpConnect, McpTransportConfig};

use super::*;
use crate::test_support::{FakeMcp, server, workspace};

async fn connected(fake: &Arc<FakeMcp>, scope: &TaskScope) -> (Arc<Client>, NoticeReceiver) {
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Http {
            url: "https://mcp.example.com".to_owned(),
            headers: Default::default(),
        },
        env: Default::default(),
    };
    let transport: Arc<dyn McpTransport> = fake.clone();
    let id = transport.connect(&workspace(), request).await.unwrap();
    Client::start(
        transport,
        id,
        &scope.spawner(),
        vec!["/Users/dev/project".to_owned()],
    )
}

#[tokio::test]
async fn requests_pair_with_responses_and_errors() {
    let fake = Arc::new(FakeMcp::default().with(
        "s",
        server(json!({}), None, |method, params| match method {
            "echo" => Some(Ok(params.clone())),
            "boom" => Some(Err((-32000, "bad".to_owned()))),
            _ => None,
        }),
    ));
    let scope = TaskScope::root("test", |_| {});
    let (client, _notices) = connected(&fake, &scope).await;
    let never = CancellationToken::new();
    let long = Duration::from_secs(5);
    assert_eq!(
        client.request("echo", json!({"a": 1}), long, &never).await,
        Ok(json!({"a": 1}))
    );
    assert_eq!(
        client.request("boom", json!({}), long, &never).await,
        Err(RpcError::Server {
            code: -32000,
            message: "bad".to_owned()
        })
    );
    // 不回答的请求：取消时发 notifications/cancelled。
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        client.request("hang", json!({}), long, &cancel).await,
        Err(RpcError::Cancelled)
    );
    let sent = fake.received();
    let cancelled = sent.last().unwrap();
    assert_eq!(cancelled.1["method"], "notifications/cancelled");
    assert_eq!(cancelled.1["params"]["requestId"], 2);
}

#[tokio::test]
async fn server_requests_notifications_and_close() {
    let fake = Arc::new(FakeMcp::default().with("s", server(json!({}), None, |_, _| None)));
    let scope = TaskScope::root("test", |_| {});
    let (client, mut notices) = connected(&fake, &scope).await;
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "id": 7, "method": "roots/list"}),
    );
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "id": 8, "method": "sampling/createMessage"}),
    );
    fake.push(
        "s",
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
    );
    assert_eq!(
        notices.recv().await,
        Some(Notice::ListChanged("tools".to_owned()))
    );
    let replies: Vec<Value> = fake
        .received()
        .into_iter()
        .map(|(_, message)| message)
        .collect();
    assert_eq!(
        replies[0],
        json!({"jsonrpc": "2.0", "id": 7, "result": {"roots": [{"uri": "file:///Users/dev/project"}]}})
    );
    assert_eq!(replies[1]["error"]["code"], -32601);
    // 关闭：等待中的请求失败，之后的请求立即失败。
    let pending = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .request(
                    "hang",
                    json!({}),
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    fake.drop_server("s", true);
    assert!(matches!(pending.await.unwrap(), Err(RpcError::Closed(_))));
    assert_eq!(
        notices.recv().await,
        Some(Notice::Closed {
            reason: Some("gone".to_owned()),
            session_expired: true
        })
    );
    assert!(client.is_closed());
}

/// 工作线 F57：`roots/list` 的 URI 同 `url.pathToFileURL(root).href`。POSIX 的期望值是用 Bun 1.3.11
/// 与 Node 24 实测的，两者逐字相同；Windows 的是 Node 24 的 `pathToFileURL(p, {windows: true})`（`C:\`
/// 取 Windows 上的结果 `file:///C:/`）。以前不编码、UNC 写成 `file:////server/share`。
#[test]
fn root_uris_are_node_file_urls() {
    for (path, expected) in [
        ("/Users/dev/project", "file:///Users/dev/project"),
        ("/a b/c", "file:///a%20b/c"),
        ("/x#y?z%w", "file:///x%23y%3Fz%25w"),
        ("/\u{fc}/\u{e9}", "file:///%C3%BC/%C3%A9"),
        ("/a\\b", "file:///a%5Cb"),
        ("/a\tb", "file:///a%09b"),
        (
            "/a\"<>`{}|^[]@!$&'()*+,;=:~",
            "file:///a%22%3C%3E%60%7B%7D%7C%5E%5B%5D@!$&'()*+,;=:%7E",
        ),
        ("/a\u{7f}-._", "file:///a%7F-._"),
        (r"C:\Work", "file:///C:/Work"),
        (r"C:\a b\c", "file:///C:/a%20b/c"),
        (r"c:\x#y?z%w", "file:///c:/x%23y%3Fz%25w"),
        (r"C:\", "file:///C:/"),
        (r"C:\a/b", "file:///C:/a/b"),
        (r"\\server\share\repo", "file://server/share/repo"),
        (r"\\SERVER\Share\a b", "file://server/Share/a%20b"),
        (r"\\?\UNC\server\share\x", "file://server/share/x"),
        (r"\\?\C:\x", "file:///C:/x"),
    ] {
        assert_eq!(file_uri(path), expected, "{path}");
    }
}

/// 按 href 去重（工作区根也出现在 `additionalDirectories` 里时只给一次）。
#[test]
fn root_uris_are_listed_once() {
    let roots = vec!["/w".to_owned(), "/x".to_owned(), "/w".to_owned()];
    assert_eq!(
        answer("roots/list", &roots).unwrap(),
        json!({"roots": [{"uri": "file:///w"}, {"uri": "file:///x"}]})
    );
}
