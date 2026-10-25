//! 连上机器时的 MCP 期望集合（R5，remote-exec.md §22.5、§22.6）：执行器结束上一个进程留下、配置里没有了的服务器，
//! 配置相同的不动；连的过程中注册表记着“正在连”，绑定的服务器等它连完。远端是本进程里的真实执行器（exec-server +
//! 内存网络），MCP 传输是记录用的替身。

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::{OwnedTask, TaskScope};
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_server::{AllowAll, ExecServer, MemoryLedger, ServerConfig};
use rust_cli_exec_transport::memory::{MemoryConnector, network};
use rust_cli_exec_transport::{Connector, LinkError, Provisioned, Provisioner, Ready};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportConfig, McpTransportEvent,
};
use rust_cli_kernel_api::testing::{UnusedEnvironment, environments};
use rust_cli_machines::{Endpoint, Machine, MachineEntry, MachineList};

use super::Fleet;
use crate::machines::MachineRegistry;

/// 执行器那一头的 MCP 传输：记下起了哪些服务器、关了哪些。
#[derive(Default)]
struct Recording {
    started: Mutex<Vec<String>>,
    closed: Mutex<Vec<String>>,
}

#[async_trait]
impl McpTransport for Recording {
    async fn connect(
        &self,
        _: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        let mut started = self.started.lock().unwrap();
        started.push(request.server.clone());
        Ok(McpConnectionId::new(format!(
            "{}-{}",
            request.server,
            started.len()
        )))
    }

    async fn send(&self, _: &McpConnectionId, _: serde_json::Value) -> Result<(), McpError> {
        Ok(())
    }

    fn events(&self, _: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        futures_util::stream::pending().boxed()
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        self.closed
            .lock()
            .unwrap()
            .push(connection.as_str().to_owned());
        Ok(())
    }
}

struct TestEndpoint {
    ready: Ready,
    connector: Arc<MemoryConnector>,
}

impl Endpoint for TestEndpoint {
    fn describe(&self) -> String {
        "test machine".to_owned()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.ready
    }

    fn connector(&self, _provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(self.connector.clone())
    }
}

fn stdio(server: &str, arg: &str) -> McpConnect {
    McpConnect {
        server: server.to_owned(),
        transport: McpTransportConfig::Stdio {
            command: "server".to_owned(),
            args: vec![arg.to_owned()],
            env: Default::default(),
            cwd: None,
        },
        env: Default::default(),
    }
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("dev-a"),
        root: EnvPath::new("/srv/proj"),
    }
}

async fn executor(mcp: Arc<Recording>) -> (Arc<MemoryConnector>, OwnedTask<()>) {
    let (connector, listener) = network("dev");
    let mut config = ServerConfig::new("0.1.60-test", "linux", "aarch64");
    config.home = Some("/home/dev".to_owned());
    config.capabilities = rust_cli_exec_wire::handshake::Capabilities::ports_and_mcp();
    let server = ExecServer::start(
        MachineEnv::new(Arc::new(UnusedEnvironment), mcp),
        Arc::new(MemoryLedger::default()),
        Arc::new(AllowAll),
        config,
    )
    .await
    .unwrap();
    let serving = rust_cli_base::owned(async move { server.serve(&listener).await });
    (Arc::new(connector), serving)
}

fn fleet(
    dir: &Path,
    connector: Arc<MemoryConnector>,
    scope: &TaskScope,
) -> (Fleet, Arc<MachineRegistry>) {
    let registry = Arc::new(MachineRegistry::new(environments(Arc::new(
        UnusedEnvironment,
    ))));
    let make = move |_: &MachineEntry| {
        let endpoint = TestEndpoint {
            ready: Ready(Provisioned {
                program: "zcode-rs".to_owned(),
                args: Vec::new(),
            }),
            connector: connector.clone(),
        };
        Ok(Machine::new(
            Arc::new(endpoint),
            ClientOptions::new("0.1.60-test", "tui-2"),
        ))
    };
    let entries = vec![MachineEntry::new("dev-a", "dev-a", Some("/srv/proj"))];
    let wanted = [(
        "dev-a".to_owned(),
        vec![stdio("browser-a", "--x"), stdio("db", "--new")],
    )];
    let fleet = Fleet::new(
        (MachineList::at(&dir.join("machines.json")), Box::new(make)),
        entries,
        registry.clone(),
        scope.spawner(),
    )
    .with_mcp(wanted.into_iter().collect());
    (fleet, registry)
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_a_machine_closes_servers_that_are_not_configured_any_more() {
    let dir = tempfile::tempdir().unwrap();
    let mcp = Arc::new(Recording::default());
    let (connector, _serving) = executor(mcp.clone()).await;
    // 上一个 TUI 进程留下的三个服务器：browser-a（配置没变）、db（配置改了）、docs（配置删了）。
    let old = ExecClient::connect(
        connector.clone(),
        ClientOptions::new("0.1.60-test", "tui-1"),
    )
    .await
    .unwrap();
    let remote = RemoteEnvironment::new(old.clone());
    for server in [
        stdio("browser-a", "--x"),
        stdio("db", "--old"),
        stdio("docs", "--y"),
    ] {
        remote.connect(&workspace(), server).await.unwrap();
    }
    old.close().await;

    let scope = TaskScope::root("fleet-mcp-test", |_| {});
    let (fleet, registry) = fleet(dir.path(), connector, &scope);
    // 连的过程中：注册表记着“正在连”（启动时的自动连接在开始之前就记上，同 `Machines::start`），绑定的服务器等它连完，
    // 不是马上“没连上”。
    let auto = registry.connecting("dev-a");
    let waiting = tokio::spawn({
        let registry = registry.clone();
        async move { registry.wait_remote("dev-a").await }
    });
    fleet.connect("dev-a").await.unwrap();
    drop(auto);
    let (_, root) = waiting.await.unwrap().unwrap();
    assert_eq!(root.root.as_str(), "/srv/proj");
    let mut closed = mcp.closed.lock().unwrap().clone();
    closed.sort();
    assert_eq!(closed, ["db-2", "docs-3"]);
    // 配置相同的接回同一个进程（不重启），缺的（新的 db）按需起。
    let (machine, workspace) = registry.remote("dev-a").unwrap();
    machine
        .mcp()
        .connect(&workspace, stdio("browser-a", "--x"))
        .await
        .unwrap();
    assert_eq!(
        mcp.started.lock().unwrap().as_slice(),
        ["browser-a", "db", "docs"]
    );
}
