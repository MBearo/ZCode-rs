//! 执行器一侧的 `${VAR}` 展开（remote-exec.md §22.7）：打开时用传输自己的环境展开，缺省不展开。
//! 服务器是 `/bin/sh` 脚本，只在 Unix 上跑。
#![cfg(unix)]

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::EnvironmentId;
use serde_json::json;

use super::*;

fn transport(expand: bool) -> (LocalMcpTransport, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let env = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("REMOTE_TOKEN".to_owned(), "t-remote".to_owned()),
    ];
    let transport = LocalMcpTransport::new(dir.path().join("ledger"), env.into(), None);
    (
        if expand {
            transport.expanding_vars()
        } else {
            transport
        },
        dir,
    )
}

/// 服务器把 `$TOKEN` 与第一个参数写成一行 JSON 就退出。
async fn report(transport: &LocalMcpTransport, root: &str) -> serde_json::Value {
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                r#"printf '{"token":"%s","arg":"%s"}\n' "$TOKEN" "$1""#.to_owned(),
                "sh".to_owned(),
                "${REMOTE_TOKEN}-${MISSING:-none}".to_owned(),
            ],
            env: [("TOKEN".to_owned(), "${REMOTE_TOKEN}".to_owned())].into(),
            cwd: None,
        },
        env: Default::default(),
    };
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("dev-a"),
        root: EnvPath::new(root),
    };
    let id = transport.connect(&workspace, request).await.unwrap();
    let mut events = transport.events(&id);
    let event = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap()
        .unwrap();
    let McpTransportEvent::Message { message } = event else {
        panic!("{event:?}");
    };
    message
}

#[tokio::test]
async fn the_executor_expands_variables_with_its_own_environment() {
    let (transport, dir) = transport(true);
    assert_eq!(
        report(&transport, dir.path().to_str().unwrap()).await,
        json!({"token": "t-remote", "arg": "t-remote-none"})
    );
}

#[tokio::test]
async fn the_control_plane_transport_does_not_expand() {
    let (transport, dir) = transport(false);
    assert_eq!(
        report(&transport, dir.path().to_str().unwrap()).await,
        json!({"token": "${REMOTE_TOKEN}", "arg": "${REMOTE_TOKEN}-${MISSING:-none}"})
    );
}

/// 远端执行 F96（remote-exec.md §23.5）：stdio 服务器的进程环境在投影之后加上（或去掉）客户端实例推来的变量，配置的
/// `env` 优先；`${VAR}` 展开先查推来的变量。
#[tokio::test]
async fn pushed_variables_reach_the_server_and_its_expansion() {
    let (transport, dir) = transport(true);
    let request = McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                r#"printf '{"sock":"%s","pinned":"%s","arg":"%s","gone":"%s"}\n' "$SSH_AUTH_SOCK" "$PINNED" "$1" "${REMOTE_TOKEN-unset}""#
                    .to_owned(),
                "sh".to_owned(),
                "${SSH_AUTH_SOCK}".to_owned(),
            ],
            env: [("PINNED".to_owned(), "from-config".to_owned())].into(),
            cwd: None,
        },
        env: [
            ("SSH_AUTH_SOCK".to_owned(), Some("/tmp/ssh-new/agent.2".to_owned())),
            ("PINNED".to_owned(), Some("pushed".to_owned())),
            ("REMOTE_TOKEN".to_owned(), None),
        ]
        .into(),
    };
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("dev-a"),
        root: EnvPath::new(dir.path().to_str().unwrap()),
    };
    let id = transport.connect(&workspace, request).await.unwrap();
    let mut events = transport.events(&id);
    let event = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap()
        .unwrap();
    let McpTransportEvent::Message { message } = event else {
        panic!("{event:?}");
    };
    assert_eq!(
        message,
        json!({"sock": "/tmp/ssh-new/agent.2", "pinned": "from-config", "arg": "/tmp/ssh-new/agent.2", "gone": "unset"})
    );
}
