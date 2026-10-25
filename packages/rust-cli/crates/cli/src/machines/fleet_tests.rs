//! Fleet（remote-exec.md §21.2）：清单的读写、连接状态与延迟拼成的快照、注册表的放进与拿走、错误的换算、变化通知。
//! 远端是本进程里的真实执行器（exec-server + 内存网络，执行端口是 kernel-api 的替身），不起子进程。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::{OwnedTask, TaskScope};
use rust_cli_exec_client::ClientOptions;
use rust_cli_exec_server::{AllowAll, ExecServer, MemoryLedger, ServerConfig};
use rust_cli_exec_transport::memory::{MemoryConnector, network};
use rust_cli_exec_transport::{
    Connector, LinkError, ProvisionError, Provisioned, Provisioner, Ready,
};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::testing::{UnusedEnvironment, environments, machine};
use rust_cli_machines::{
    Endpoint, Machine, MachineEntry, MachineError, MachineList, MachineState, MachinesError,
};
use tokio::sync::watch;

use super::{Changed, Fleet, FleetError, MakeMachine};
use crate::machines::MachineRegistry;

/// 一台“远端”：内存网络另一头的执行器。
struct Remote {
    connector: Arc<MemoryConnector>,
    _serving: OwnedTask<()>,
}

async fn remote(name: &str) -> Remote {
    let (connector, listener) = network(name);
    let mut config = ServerConfig::new("0.1.58-test", "linux", "aarch64");
    config.home = Some("/home/dev".to_owned());
    let server = ExecServer::start(
        machine(Arc::new(UnusedEnvironment)),
        Arc::new(MemoryLedger::default()),
        Arc::new(AllowAll),
        config,
    )
    .await
    .unwrap();
    let serving = rust_cli_base::owned(async move { server.serve(&listener).await });
    Remote {
        connector: Arc::new(connector),
        _serving: serving,
    }
}

/// 端点：部署成功（或按 `fail` 失败），连接器是内存网络。
struct TestEndpoint {
    ready: Ready,
    fail: Option<ProvisionError>,
    connector: Arc<MemoryConnector>,
}

#[async_trait::async_trait]
impl Provisioner for TestEndpoint {
    async fn provision(&self) -> Result<Provisioned, ProvisionError> {
        match &self.fail {
            Some(error) => Err(error.clone()),
            None => self.ready.provision().await,
        }
    }
}

impl Endpoint for TestEndpoint {
    fn describe(&self) -> String {
        "test machine".to_owned()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        self
    }

    fn connector(&self, _provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(self.connector.clone())
    }
}

/// 按名字建状态机：`bad` 部署失败（连不上），其余连到 `remote`。
fn make(remote: &Remote) -> MakeMachine {
    let connector = remote.connector.clone();
    Box::new(move |entry: &MachineEntry| {
        let fail = (entry.name == "bad").then(|| ProvisionError::Unreachable {
            message: "connection refused".to_owned(),
            target: "bad.example".to_owned(),
        });
        let endpoint = TestEndpoint {
            ready: Ready(Provisioned {
                program: "zcode-rs".to_owned(),
                args: Vec::new(),
            }),
            fail,
            connector: connector.clone(),
        };
        Ok(Machine::new(
            Arc::new(endpoint),
            ClientOptions::new("0.1.58-test", "client-1"),
        ))
    })
}

struct Built {
    fleet: Fleet,
    registry: Arc<MachineRegistry>,
    list: MachineList,
    _scope: TaskScope,
    _remote: Remote,
}

async fn built(dir: &Path) -> Built {
    let remote = remote("dev").await;
    let list = MachineList::at(&dir.join("machines.json"));
    let registry = Arc::new(MachineRegistry::new(environments(Arc::new(
        UnusedEnvironment,
    ))));
    let scope = TaskScope::root("fleet-test", |_| {});
    let fleet = Fleet::new(
        (list.clone(), make(&remote)),
        Vec::new(),
        registry.clone(),
        scope.spawner(),
    );
    Built {
        fleet,
        registry,
        list,
        _scope: scope,
        _remote: remote,
    }
}

/// 等到快照满足条件（每次变化通知后重看；有上限，只为失败的测试不挂住）。
async fn until(fleet: &Fleet, mut changes: watch::Receiver<u64>, done: impl Fn(&Fleet) -> bool) {
    let wait = async {
        while !done(fleet) {
            changes.changed().await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .expect("the snapshot did not change as expected");
}

#[tokio::test(flavor = "multi_thread")]
async fn add_connect_disconnect_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    // 执行器与任务作用域要活到测试结束（`..` 会把没绑定的字段立刻丢掉）。
    let Built {
        fleet,
        registry,
        list,
        _scope,
        _remote,
    } = built(dir.path()).await;
    let changes = fleet.subscribe();
    fleet
        .add(MachineEntry::new("dev-a", "dev-a", Some("/srv/proj")))
        .await
        .unwrap();
    let seen = fleet.snapshot();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].entry.working_directory.as_deref(),
        Some("/srv/proj")
    );
    assert_eq!(seen[0].state, MachineState::Unconnected);
    assert!(
        changes.has_changed().unwrap(),
        "adding wakes the subscribers"
    );

    assert_eq!(fleet.connect("dev-a").await.unwrap(), Changed::Done);
    let seen = &fleet.snapshot()[0];
    assert!(matches!(seen.state, MachineState::Ready(_)), "{seen:?}");
    let welcome = seen.welcome.as_ref().unwrap();
    assert_eq!(
        (
            welcome.os.as_str(),
            welcome.arch.as_str(),
            welcome.server_version.as_str()
        ),
        ("linux", "aarch64", "0.1.58-test")
    );
    // 进了注册表（工作区是清单里的工作目录），清单记下连着。
    let (_, workspace) = registry.remote("dev-a").expect("in the registry");
    assert_eq!(workspace.root.as_str(), "/srv/proj");
    assert!(list.load().await.unwrap()[0].connected);
    // 握手后马上一次心跳：延迟到了就唤醒订阅方。
    until(&fleet, fleet.subscribe(), |fleet| {
        fleet.snapshot()[0].latency.is_some()
    })
    .await;
    assert_eq!(fleet.connect("dev-a").await.unwrap(), Changed::Already);

    assert_eq!(fleet.disconnect("dev-a").await.unwrap(), Changed::Done);
    assert_eq!(fleet.snapshot()[0].state, MachineState::Unconnected);
    assert!(fleet.snapshot()[0].latency.is_none());
    assert!(registry.remote("dev-a").is_none());
    assert!(!list.load().await.unwrap()[0].connected);
    assert_eq!(fleet.disconnect("dev-a").await.unwrap(), Changed::Already);

    fleet.connect("dev-a").await.unwrap();
    fleet.remove("dev-a").await.unwrap();
    assert!(fleet.snapshot().is_empty());
    assert!(registry.remote("dev-a").is_none());
    assert!(list.load().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn problems_come_back_typed() {
    let dir = tempfile::tempdir().unwrap();
    let Built {
        fleet,
        list,
        _scope,
        _remote,
        ..
    } = built(dir.path()).await;
    fleet
        .add(MachineEntry::new("bad", "bad.example", None))
        .await
        .unwrap();
    let error = fleet.connect("bad").await.unwrap_err();
    assert!(
        matches!(&error, FleetError::Connection(MachineError::Unreachable(m)) if m == "connection refused"),
        "{error:?}"
    );
    assert!(matches!(
        fleet.snapshot()[0].state,
        MachineState::Failed {
            error: MachineError::Unreachable(_)
        }
    ));
    // 连不上不改清单的 `connected`。
    assert!(!list.load().await.unwrap()[0].connected);
    // 断开失败的机器：回到没连，算“本来就没连着”。
    assert_eq!(fleet.disconnect("bad").await.unwrap(), Changed::Already);
    assert_eq!(fleet.snapshot()[0].state, MachineState::Unconnected);
    let exists = fleet
        .add(MachineEntry::new("bad", "other", None))
        .await
        .unwrap_err();
    assert!(matches!(exists, FleetError::List(MachinesError::Exists(_))));
    let name = fleet
        .add(MachineEntry::new("local", "x", None))
        .await
        .unwrap_err();
    assert!(matches!(name, FleetError::List(MachinesError::Name(_))));
    for missing in [
        fleet.connect("nope").await.unwrap_err(),
        fleet.disconnect("nope").await.unwrap_err(),
        fleet.remove("nope").await.unwrap_err(),
    ] {
        assert!(
            matches!(&missing, FleetError::List(MachinesError::Missing(name)) if name == "nope"),
            "{missing:?}"
        );
    }
}

/// 别的进程加进清单的机器：内存里没有时重读文件，照样能连；`remembered` 列出 `connected` 的机器。
#[tokio::test(flavor = "multi_thread")]
async fn machines_added_elsewhere_are_found() {
    let dir = tempfile::tempdir().unwrap();
    let Built {
        fleet,
        list,
        _scope,
        _remote,
        ..
    } = built(dir.path()).await;
    let mut entry = MachineEntry::new("dev-b", "dev-b", None);
    entry.connected = true;
    list.add(entry).await.unwrap();
    assert!(fleet.snapshot().is_empty());
    assert_eq!(fleet.connect("dev-b").await.unwrap(), Changed::Done);
    assert_eq!(fleet.snapshot()[0].entry.name, "dev-b");
    assert_eq!(fleet.remembered(), ["dev-b"]);
}

/// 同时来的两个连接请求（启动时的自动连接与用户的 `/machines connect`）：一个连，另一个等它结束，得到“已经连着”，
/// 不报“正在部署时不能连”的错。
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_connects_share_one_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let Built {
        fleet,
        _scope,
        _remote,
        ..
    } = built(dir.path()).await;
    fleet
        .add(MachineEntry::new("dev-a", "dev-a", None))
        .await
        .unwrap();
    let (first, second) = tokio::join!(fleet.connect("dev-a"), fleet.connect("dev-a"));
    let mut results = [first.unwrap(), second.unwrap()];
    results.sort_by_key(|changed| *changed == Changed::Already);
    assert_eq!(results, [Changed::Done, Changed::Already]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_knows_the_listed_machines() {
    // F98（remote-exec.md §25.2.2）：注册表的 `listed` 读 Fleet 内存里的清单（不另存一份）：加进清单就有，没连也算；
    // 删掉就没有。旧代码的注册表不认识清单（缺省 false）。
    let dir = tempfile::tempdir().unwrap();
    let Built {
        fleet,
        registry,
        _scope,
        _remote,
        ..
    } = built(dir.path()).await;
    assert!(!registry.listed("dev-a"));
    fleet
        .add(MachineEntry::new("dev-a", "dev-a", None))
        .await
        .unwrap();
    assert!(registry.listed("dev-a"));
    assert!(registry.remote("dev-a").is_none(), "listed, not connected");
    assert!(!registry.listed("gpu-1"));
    fleet.remove("dev-a").await.unwrap();
    assert!(!registry.listed("dev-a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn retrying_now_only_acts_on_a_machine_waiting_to_reconnect() {
    // F98（remote-exec.md §25.4.1）：没连过、连着时“立即重试”什么都不做（`Already`）；不在清单里时是“没有这台”。
    // 叫醒退避的那一步在 exec-client 的测试里（`retrying_now_skips_the_rest_of_the_backoff`）。
    let dir = tempfile::tempdir().unwrap();
    let Built {
        fleet,
        _scope,
        _remote,
        ..
    } = built(dir.path()).await;
    assert!(matches!(
        fleet.retry("dev-a").await,
        Err(FleetError::List(MachinesError::Missing(name))) if name == "dev-a"
    ));
    fleet
        .add(MachineEntry::new("dev-a", "dev-a", None))
        .await
        .unwrap();
    assert_eq!(fleet.retry("dev-a").await.unwrap(), Changed::Already);
    assert_eq!(fleet.connect("dev-a").await.unwrap(), Changed::Done);
    assert_eq!(fleet.retry("dev-a").await.unwrap(), Changed::Already);
    fleet.disconnect("dev-a").await.unwrap();
}
