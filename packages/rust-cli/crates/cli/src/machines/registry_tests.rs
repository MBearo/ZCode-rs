//! 注册表的“正在连”（R5，remote-exec.md §22.6）：`wait_remote` 在正在连时等它连上或放弃；没在连时不等。

use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, MachineFacts, RemoteMachine};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::{UnusedEnvironment, environments, machine};

use rust_cli_exec_client::ConnectionState;
use tokio::sync::watch;

use super::MachineRegistry;

fn info(name: &str) -> RemoteMachine {
    RemoteMachine {
        name: name.to_owned(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(name),
            root: EnvPath::new("/srv/proj"),
        },
        facts: MachineFacts {
            os: "linux".to_owned(),
            arch: "aarch64".to_owned(),
            shell: "bash".to_owned(),
            home: None,
            path_style: PathStyle::Posix,
        },
    }
}

/// 一条连着的连接的状态（exec-client 的 `states()`）。
fn connected_link() -> (
    watch::Sender<ConnectionState>,
    watch::Receiver<ConnectionState>,
) {
    watch::channel(ConnectionState::Connected {
        epoch: 1,
        server_instance: "exec-1".to_owned(),
    })
}

fn registry() -> Arc<MachineRegistry> {
    Arc::new(MachineRegistry::new(environments(Arc::new(
        UnusedEnvironment,
    ))))
}

#[tokio::test]
async fn a_machine_that_is_not_connecting_is_not_waited_for() {
    let registry = registry();
    assert!(registry.wait_remote("dev-a").await.is_none());
}

#[tokio::test]
async fn waiting_ends_when_the_machine_connects() {
    let registry = registry();
    let connecting = registry.connecting("dev-a");
    let waiting = tokio::spawn({
        let registry = registry.clone();
        async move { registry.wait_remote("dev-a").await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiting.is_finished());
    registry.connected(
        machine(Arc::new(UnusedEnvironment)).named("dev-a"),
        info("dev-a"),
        connected_link().1,
    );
    drop(connecting);
    let (found, workspace) = waiting.await.unwrap().unwrap();
    assert_eq!(found.name(), "dev-a");
    assert_eq!(workspace.root.as_str(), "/srv/proj");
}

#[tokio::test]
async fn waiting_ends_when_the_connection_gives_up() {
    let registry = registry();
    let first = registry.connecting("dev-a");
    let second = registry.connecting("dev-a");
    let waiting = tokio::spawn({
        let registry = registry.clone();
        async move { registry.wait_remote("dev-a").await }
    });
    drop(first);
    tokio::time::sleep(Duration::from_millis(20)).await;
    // 还有一个请求在连。
    assert!(!waiting.is_finished());
    drop(second);
    assert!(waiting.await.unwrap().is_none());
}

/// 远端执行 F96（remote-exec.md §23.3）：“在不在重连”直接读那台机器的 exec-client 的连接状态，不另存一份；重连中的
/// 机器还在注册表里。
#[test]
fn reconnecting_follows_the_connection_state_of_the_machine() {
    let registry = registry();
    let (link, state) = connected_link();
    registry.connected(
        machine(Arc::new(UnusedEnvironment)).named("dev-a"),
        info("dev-a"),
        state,
    );
    assert!(!registry.reconnecting("dev-a"));
    link.send_replace(ConnectionState::Reconnecting {
        attempt: 0,
        reason: "link closed".to_owned(),
        retry_at: None,
    });
    assert!(registry.reconnecting("dev-a"));
    assert!(registry.remote("dev-a").is_some());
    link.send_replace(ConnectionState::Connected {
        epoch: 2,
        server_instance: "exec-1".to_owned(),
    });
    assert!(!registry.reconnecting("dev-a"));
    link.send_replace(ConnectionState::Reconnecting {
        attempt: 1,
        reason: "link closed".to_owned(),
        retry_at: None,
    });
    registry.disconnected("dev-a");
    assert!(!registry.reconnecting("dev-a"));
    assert!(!registry.reconnecting("gpu-1"));
}
