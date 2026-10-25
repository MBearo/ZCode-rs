//! 绑定到远端机器的服务器（R5，remote-exec.md §22.6）：管道与工作区取那台机器的；没连上时失败（R3 的文字）；那台机器
//! 正在连时等它；`local` 是本机；`/mcp` 的快照带机器名。ext 的语义（握手、工具、调用）不变。

use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::testing::UnusedEnvironment;
use serde_json::json;
use tokio::sync::watch;

use super::*;
use crate::mcp::McpScope;
use crate::test_support::{FakeMcp, server, workspace};

/// 一台远端机器 dev-a（管道是给的替身）；`ready` 为 false 时它“正在连”，`wait_remote` 等它变成 true。
struct Machines {
    remote: MachineEnv,
    ready: watch::Sender<bool>,
    known: Mutex<Vec<String>>,
}

fn remote_workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("dev-a"),
        root: EnvPath::new("/srv/proj"),
    }
}

impl Machines {
    fn new(fake: Arc<FakeMcp>, ready: bool) -> Arc<Self> {
        Arc::new(Self {
            remote: MachineEnv::new(Arc::new(UnusedEnvironment), fake).named("dev-a"),
            ready: watch::channel(ready).0,
            known: Mutex::new(vec!["dev-a".to_owned()]),
        })
    }
}

impl EnvironmentRegistry for Machines {
    fn local_machine(&self) -> MachineEnv {
        MachineEnv::new(
            Arc::new(UnusedEnvironment),
            Arc::new(rust_cli_kernel_api::mcp::NoMcpTransport),
        )
    }

    fn local(&self) -> LocalServices {
        LocalServices::new(Arc::new(UnusedEnvironment))
    }

    fn remote(&self, name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
        (name == "dev-a" && *self.ready.borrow()).then(|| (self.remote.clone(), remote_workspace()))
    }

    fn wait_remote(&self, name: &str) -> BoxFuture<'static, Option<(MachineEnv, WorkspaceRef)>> {
        if !self.known.lock().unwrap().iter().any(|known| known == name) {
            return async { None }.boxed();
        }
        let mut ready = self.ready.subscribe();
        let found = (self.remote.clone(), remote_workspace());
        async move {
            ready.wait_for(|ready| *ready).await.ok()?;
            Some(found)
        }
        .boxed()
    }
}

fn bound(name: &str, machine: Option<&str>) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::User,
        transport: McpTransportConfig::Stdio {
            command: "npx".to_owned(),
            args: vec!["@playwright/mcp".to_owned()],
            env: Default::default(),
            cwd: None,
        },
        options: Default::default(),
        machine: machine.map(str::to_owned),
    }
}

fn browser() -> Arc<crate::test_support::Handler> {
    server(json!({"tools": {}}), None, |method, _| match method {
        "tools/list" => Some(Ok(
            json!({"tools": [{"name": "navigate", "inputSchema": {"type": "object"}}]}),
        )),
        _ => None,
    })
}

fn manager(local: Arc<FakeMcp>, machines: Arc<Machines>, scope: &TaskScope) -> McpManager {
    let manager = McpManager::new(
        local,
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    );
    manager.set_machines(machines);
    manager
}

#[tokio::test]
async fn a_bound_server_uses_the_pipe_and_workspace_of_its_machine() {
    let local = Arc::new(FakeMcp::default().with("here", browser()));
    let remote = Arc::new(FakeMcp::default().with("browser-a", browser()));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(local.clone(), Machines::new(remote.clone(), true), &scope);
    manager.start(
        workspace(),
        vec![
            bound("browser-a", Some("dev-a")),
            bound("here", Some("local")),
        ],
    );
    manager.wait_ready().await;
    assert_eq!(
        manager.status(),
        [
            ("browser-a".to_owned(), McpStatus::Connected),
            ("here".to_owned(), McpStatus::Connected),
        ]
    );
    // 管道：绑定的服务器只在 dev-a 的传输上连，工作区是 dev-a 的工作目录；`local` 是本机。
    assert_eq!(
        remote.roots(),
        [("browser-a".to_owned(), "/srv/proj".to_owned())]
    );
    assert_eq!(
        local.roots(),
        [("here".to_owned(), crate::test_support::CWD.to_owned())]
    );
    // 工具名与本机的服务器同一个规则（MCP 工具不加 machine 参数）。
    let names: Vec<String> = manager
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(names, ["mcp__browser-a__navigate", "mcp__here__navigate"]);
    let infos = manager.server_infos();
    assert_eq!(infos[0].machine.as_deref(), Some("dev-a"));
    assert_eq!(infos[1].machine, None);
}

#[tokio::test]
async fn a_server_bound_to_a_machine_that_is_not_connected_fails() {
    let remote = Arc::new(FakeMcp::default().with("browser-b", browser()));
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(
        Arc::new(FakeMcp::default()),
        Machines::new(remote.clone(), true),
        &scope,
    );
    manager.start(workspace(), vec![bound("browser-b", Some("gpu-1"))]);
    manager.wait_ready().await;
    assert_eq!(
        manager.status(),
        [(
            "browser-b".to_owned(),
            McpStatus::Failed {
                reason: "Machine \"gpu-1\" is not connected.".to_owned()
            }
        )]
    );
    assert!(remote.roots().is_empty());
}

#[tokio::test]
async fn a_server_waits_for_its_machine_while_it_is_connecting() {
    let remote = Arc::new(FakeMcp::default().with("browser-a", browser()));
    let machines = Machines::new(remote.clone(), false);
    let scope = TaskScope::root("test", |_| {});
    let manager = manager(Arc::new(FakeMcp::default()), machines.clone(), &scope);
    manager.start(workspace(), vec![bound("browser-a", Some("dev-a"))]);
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(
        manager.status(),
        [("browser-a".to_owned(), McpStatus::Pending)]
    );
    machines.ready.send_replace(true);
    manager.wait_ready().await;
    assert_eq!(
        manager.status(),
        [("browser-a".to_owned(), McpStatus::Connected)]
    );
    // 没有注册表的组装（只给测试）：绑定的服务器连不上。
    let alone = McpManager::new(
        Arc::new(FakeMcp::default()),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    );
    alone.start(workspace(), vec![bound("browser-a", Some("dev-a"))]);
    alone.wait_ready().await;
    assert!(matches!(alone.status()[0].1, McpStatus::Failed { .. }));
}

/// 一台后来才连上的机器 dev-a：连上之前不在注册表里（`wait_remote` 不等：没在连），`arrive` 之后在，并发出连接变化。
struct Arriving {
    remote: MachineEnv,
    here: watch::Sender<bool>,
}

impl Arriving {
    fn new(fake: Arc<FakeMcp>) -> Arc<Self> {
        Arc::new(Self {
            remote: MachineEnv::new(Arc::new(UnusedEnvironment), fake).named("dev-a"),
            here: watch::channel(false).0,
        })
    }

    fn arrive(&self, here: bool) {
        self.here.send_replace(here);
    }
}

impl EnvironmentRegistry for Arriving {
    fn local_machine(&self) -> MachineEnv {
        MachineEnv::new(
            Arc::new(UnusedEnvironment),
            Arc::new(rust_cli_kernel_api::mcp::NoMcpTransport),
        )
    }

    fn local(&self) -> LocalServices {
        LocalServices::new(Arc::new(UnusedEnvironment))
    }

    fn machines(&self) -> rust_cli_kernel_api::env_registry::MachineSet {
        use rust_cli_kernel_api::env_registry::{MachineFacts, MachineSet, RemoteMachine};
        let here = *self.here.borrow();
        MachineSet {
            version: u64::from(here),
            machines: here
                .then(|| RemoteMachine {
                    name: "dev-a".to_owned(),
                    workspace: remote_workspace(),
                    facts: MachineFacts {
                        os: "linux".to_owned(),
                        arch: "aarch64".to_owned(),
                        shell: "/bin/bash".to_owned(),
                        home: None,
                        path_style: rust_cli_kernel_api::env_files::PathStyle::Posix,
                    },
                })
                .into_iter()
                .collect(),
        }
    }

    fn remote(&self, name: &str) -> Option<(MachineEnv, WorkspaceRef)> {
        (name == "dev-a" && *self.here.borrow()).then(|| (self.remote.clone(), remote_workspace()))
    }

    fn changes(&self) -> futures_util::stream::BoxStream<'static, u64> {
        use futures_util::StreamExt;
        futures_util::stream::unfold(self.here.subscribe(), |mut here| async move {
            here.changed().await.ok()?;
            let version = u64::from(*here.borrow_and_update());
            Some((version, here))
        })
        .boxed()
    }
}

/// 等到状态满足条件（只为失败的测试不挂住）。
async fn until_status(manager: &McpManager, done: impl Fn(&[(String, McpStatus)]) -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !done(&manager.status()) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("status stayed {:?}", manager.status()));
}

/// F98（remote-exec.md §25.5，Docker 试用 L6）：机器还不在注册表里时绑定到它的服务器失败；机器进注册表时（`/machines add`
/// 连上）它自动重连一次。绑定到别的机器、本机的服务器不动。旧代码要手动 `/mcp reconnect`。
#[tokio::test]
async fn a_failed_bound_server_reconnects_when_its_machine_arrives() {
    let remote = Arc::new(FakeMcp::default().with("browser-a", browser()));
    let machines = Arriving::new(remote.clone());
    let scope = TaskScope::root("test", |_| {});
    let manager = McpManager::new(
        Arc::new(FakeMcp::default()),
        scope.spawner(),
        McpSettings::default(),
        crate::clock_tests::TokioClock::new(),
    );
    manager.set_machines(machines.clone());
    manager.start(
        workspace(),
        vec![
            bound("browser-a", Some("dev-a")),
            bound("browser-b", Some("gpu-1")),
        ],
    );
    manager.wait_ready().await;
    let failed = |machine: &str| McpStatus::Failed {
        reason: format!("Machine \"{machine}\" is not connected."),
    };
    assert_eq!(
        manager.status(),
        [
            ("browser-a".to_owned(), failed("dev-a")),
            ("browser-b".to_owned(), failed("gpu-1")),
        ]
    );
    machines.arrive(true);
    until_status(&manager, |status| status[0].1 == McpStatus::Connected).await;
    assert_eq!(manager.status()[1].1, failed("gpu-1"));
    assert_eq!(
        remote.roots(),
        [("browser-a".to_owned(), "/srv/proj".to_owned())]
    );
}
