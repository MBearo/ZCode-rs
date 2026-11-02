//! 注册表与机器的组装（remote-exec.md §15.8.1）：R0 只有本机；一台机器的端口都落到同一个对象。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};

use super::{EnvironmentRegistry, PrimaryRegistry};
use crate::controller_services::{ControllerServices, Fetch};
use crate::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::env_git::GitStatusRequest;
use crate::env_shell::BackgroundTask;
use crate::ids::{EnvironmentId, OperationId, SessionId, ShellTaskId};
use crate::ids::{MachineName, PRIMARY_MACHINE};
use crate::machine::Git;
use crate::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, MachineEnv, Pdf, Search, Shell,
};
use crate::mcp::NoMcpTransport;

/// 每个端口的调用都记到同一个计数上：看得出各端口是不是同一个对象。
#[derive(Default)]
struct Counting(AtomicUsize);

impl Counting {
    fn hit(&self) -> EnvError {
        self.0.fetch_add(1, Ordering::Relaxed);
        EnvError::Unsupported {
            what: "counting".to_owned(),
        }
    }
}

#[async_trait]
impl EnvInfo for Counting {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(self.hit())
    }
}

#[async_trait]
impl Files for Counting {
    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        _request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        Err(self.hit())
    }

    async fn write_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(self.hit())
    }
}

#[async_trait]
impl Commands for Counting {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        Err(self.hit())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Err(self.hit())]).boxed()
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Err(self.hit())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Err(self.hit())
    }
}

#[async_trait]
impl Shell for Counting {
    async fn ack_shell_notice(&self, _session: &SessionId, _seq: u64) -> Result<(), EnvError> {
        Err(self.hit())
    }
}

#[async_trait]
impl Background for Counting {
    async fn background_elapsed(&self, _request: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Err(self.hit())
    }
}

#[async_trait]
impl Checkpoints for Counting {
    async fn rewind_finish(&self, _token: &OperationId) -> Result<(), EnvError> {
        Err(self.hit())
    }
}

impl Search for Counting {}
impl Pdf for Counting {}

#[async_trait]
impl Git for Counting {
    async fn worktree_list(
        &self,
        _cwd: EnvPath,
    ) -> Result<crate::env_worktree::WorktreeList, EnvError> {
        Err(self.hit())
    }
}

impl Fetch for Counting {}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/repo"),
    }
}

fn registry(env: &Arc<Counting>) -> PrimaryRegistry {
    PrimaryRegistry::new(
        MachineEnv::new(env.clone(), Arc::new(NoMcpTransport)),
        ControllerServices::new(env.clone()),
    )
}

#[test]
fn the_registry_only_knows_the_local_machine() {
    let registry = registry(&Arc::new(Counting::default()));
    // 名字是身份（deployment.md §23）；以前的名字仍指主 env。
    assert_eq!(PRIMARY_MACHINE, "primary");
    assert!(
        MachineName::is_primary_name("primary")
            && MachineName::is_primary_name("local")
            && !MachineName::is_primary_name("dev-a")
    );
    assert!(registry.machine(&MachineName::primary()).is_some());
    assert!(registry.machine(&MachineName::new("dev-a")).is_none());
    // 审查 K9：空串与别处一样是主 env（以前这里查不到，别的地方当主 env；产品代码里没有按空串查的调用方）。
    assert!(
        registry
            .machine(&MachineName::new(""))
            .is_some_and(|machine| machine.is_primary())
    );
}

#[tokio::test]
async fn every_port_of_a_machine_reaches_the_same_object() {
    let env = Arc::new(Counting::default());
    let registry = registry(&env);
    let Some(machine) = registry.machine(&MachineName::primary()) else {
        panic!("the local machine is always there");
    };
    let session = SessionId::new("sess_1");
    let token = OperationId::new("op_1");
    let task = BackgroundTask {
        session: session.clone(),
        task: ShellTaskId::new("t1"),
    };
    let _ = machine.info().facts(&workspace()).await;
    let request = ReadFile {
        path: EnvPath::new("/Users/dev/repo/a.txt"),
    };
    let _ = machine.files().read_file(&workspace(), request).await;
    let _ = machine.commands().cancel(token.clone()).await;
    let _ = machine.shell().ack_shell_notice(&session, 1).await;
    let _ = machine.background().background_elapsed(task).await;
    let _ = machine.checkpoints().rewind_finish(&token).await;
    let _ = registry
        .primary_machine()
        .git()
        .worktree_list(EnvPath::new("/Users/dev/repo"))
        .await;
    let _ = registry
        .primary_machine()
        .commands()
        .close_session(&session, false)
        .await;
    assert_eq!(env.0.load(Ordering::Relaxed), 8);
}

#[tokio::test]
async fn ports_without_an_override_keep_the_unsupported_default() {
    let env = Arc::new(Counting::default());
    let registry = registry(&env);
    let machine = registry.primary_machine();
    let request = GitStatusRequest {
        cwd: EnvPath::new("/Users/dev/repo"),
    };
    let status = machine.git().git_status(&workspace(), request).await;
    assert_eq!(
        status,
        Err(EnvError::Unsupported {
            what: "git_status".to_owned()
        })
    );
    let request = crate::env_pdf::PdfPageCount {
        path: EnvPath::new("/Users/dev/repo/a.pdf"),
    };
    let page = machine.pdf().pdf_page_count(&workspace(), request).await;
    assert!(matches!(page, Err(EnvError::Unsupported { what }) if what == "pdf_page_count"));
    assert_eq!(env.0.load(Ordering::Relaxed), 0);
}

#[test]
fn a_machine_carries_its_name() {
    let env = Arc::new(Counting::default());
    let primary = MachineEnv::new(env, Arc::new(NoMcpTransport));
    assert_eq!(primary.name().as_str(), PRIMARY_MACHINE);
    assert!(primary.is_primary());
    let remote = primary.named(MachineName::new("dev-a"));
    assert_eq!(remote.name().as_str(), "dev-a");
    assert!(!remote.is_primary());
}

#[tokio::test]
async fn an_unavailable_machine_says_it_is_not_connected() {
    let machine = MachineEnv::unavailable("dev-a");
    assert_eq!(machine.name().as_str(), "dev-a");
    let error = machine.info().facts(&workspace()).await.unwrap_err();
    assert_eq!(
        error,
        EnvError::Io {
            message: "Machine \"dev-a\" is not connected.".to_owned()
        }
    );
    let session = SessionId::new("sess_1");
    assert!(
        machine
            .commands()
            .close_session(&session, false)
            .await
            .is_err()
    );
}

#[test]
fn the_default_lookup_goes_through_remote() {
    let env = Arc::new(Counting::default());
    let registry = crate::testing::TestMachines::new(Arc::new(crate::testing::UnusedEnvironment));
    assert!(registry.machine(&MachineName::new("dev-a")).is_none());
    assert!(registry.machines().is_empty());
    registry.connect("dev-a", env, "/home/dev/proj");
    let found = registry.machine(&MachineName::new("dev-a")).unwrap();
    assert_eq!(found.name().as_str(), "dev-a");
    let set = registry.machines();
    assert_eq!(set.version, 1);
    assert_eq!(set.names(), ["dev-a"]);
    assert_eq!(
        set.get("dev-a").unwrap().workspace.root.as_str(),
        "/home/dev/proj"
    );
    assert_eq!(
        registry
            .remote(&MachineName::new("dev-a"))
            .unwrap()
            .1
            .environment
            .as_str(),
        "dev-a"
    );
    registry.disconnect("dev-a");
    assert!(registry.machine(&MachineName::new("dev-a")).is_none());
    assert_eq!(registry.machines().version, 2);
}

#[tokio::test]
async fn stat_many_defaults_to_one_stat_per_path() {
    let env = Arc::new(Counting::default());
    let machine = MachineEnv::new(env, Arc::new(NoMcpTransport));
    let request = crate::env_files::StatMany {
        paths: vec![EnvPath::new("/a"), EnvPath::new("/b")],
    };
    let found = machine
        .files()
        .stat_many(&workspace(), request)
        .await
        .unwrap();
    // Counting 没有覆盖 stat：每项都是缺省的 Unsupported。
    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .all(|item| matches!(item, Err(EnvError::Unsupported { what }) if what == "stat"))
    );
}

#[tokio::test]
async fn waiting_for_a_remote_machine_does_not_wait_by_default() {
    // R5（remote-exec.md §22.9）：缺省的 wait_remote 立即给出 remote(name) 的结果，没在连的机器不等。
    let registry = crate::testing::TestMachines::new(Arc::new(crate::testing::UnusedEnvironment));
    assert!(
        registry
            .wait_remote(&MachineName::new("dev-a"))
            .await
            .is_none()
    );
    registry.connect("dev-a", Arc::new(Counting::default()), "/home/dev/proj");
    let (machine, workspace) = registry
        .wait_remote(&MachineName::new("dev-a"))
        .await
        .unwrap();
    assert_eq!(machine.name().as_str(), "dev-a");
    assert_eq!(workspace.root.as_str(), "/home/dev/proj");
}

#[test]
fn reconnecting_is_false_unless_the_registry_says_so() {
    // F96（remote-exec.md §23.3）：只有本机的注册表没有重连中的机器；替身按测试的设置答，机器断开时一起忘掉。
    let env = Arc::new(Counting::default());
    assert!(!registry(&env).reconnecting(&MachineName::new("dev-a")));
    let machines = crate::testing::TestMachines::new(Arc::new(crate::testing::UnusedEnvironment));
    machines.connect("dev-a", env, "/home/dev/proj");
    assert!(!machines.reconnecting(&MachineName::new("dev-a")));
    machines.set_reconnecting("dev-a", true);
    assert!(machines.reconnecting(&MachineName::new("dev-a")));
    assert!(
        machines.remote(&MachineName::new("dev-a")).is_some(),
        "a reconnecting machine stays"
    );
    machines.disconnect("dev-a");
    assert!(!machines.reconnecting(&MachineName::new("dev-a")));
}

#[test]
fn listed_machines_are_known_even_when_they_are_not_connected() {
    // F98（remote-exec.md §25.2.2）：只有本机的注册表不认识任何清单里的机器；替身连上的与登记的都算，断开之后仍算。
    let env = Arc::new(Counting::default());
    assert!(!registry(&env).listed(&MachineName::new("dev-a")));
    let machines = crate::testing::TestMachines::new(Arc::new(crate::testing::UnusedEnvironment));
    assert!(!machines.listed(&MachineName::new("dev-a")));
    machines.connect("dev-a", env, "/home/dev/proj");
    assert!(machines.listed(&MachineName::new("dev-a")));
    machines.disconnect("dev-a");
    assert!(
        machines.listed(&MachineName::new("dev-a")),
        "a disconnected machine stays listed"
    );
    assert!(machines.remote(&MachineName::new("dev-a")).is_none());
    machines.list("gpu-1");
    assert!(machines.listed(&MachineName::new("gpu-1")));
    assert!(
        machines.remote(&MachineName::new("gpu-1")).is_none(),
        "listing does not connect"
    );
    assert!(!machines.listed(&MachineName::new("ci-3")));
}

/// 机器名的规则只有一处（审查 K9）：`primary`、旧名 `local` 与空串都是主 env，构造之后存成 `primary`；读的时候也经它。
/// 序列化是名字本身，DTO 里主 env 不写出。
#[test]
fn machine_names_have_one_rule_for_the_primary_env() {
    for name in ["primary", "local", ""] {
        let machine = MachineName::new(name);
        assert!(machine.is_primary(), "{name:?}");
        assert_eq!(machine.as_str(), PRIMARY_MACHINE);
        assert_eq!(machine.remote(), None);
    }
    let remote = MachineName::new("dev-a");
    assert!(!remote.is_primary());
    assert_eq!(remote.remote(), Some("dev-a"));
    assert_eq!(MachineName::default(), MachineName::primary());
    assert_eq!(serde_json::to_string(&remote).unwrap(), r#""dev-a""#);
    assert_eq!(
        serde_json::from_str::<MachineName>(r#""local""#).unwrap(),
        MachineName::primary()
    );
    assert_eq!(
        serde_json::from_str::<MachineName>(r#""dev-a""#).unwrap(),
        remote
    );
}

/// DTO 的字节与以前的 `Option<String>` 相同：主 env 不写出、没有这个键读成主 env，额外机器写名字。
#[test]
fn a_machine_field_serializes_as_the_previous_option() {
    use crate::session_api::FileChange;
    let local = FileChange {
        path: "a.txt".to_owned(),
        additions: 1,
        deletions: 0,
        machine: MachineName::primary(),
    };
    let text = serde_json::to_string(&local).unwrap();
    assert_eq!(text, r#"{"path":"a.txt","additions":1,"deletions":0}"#);
    assert_eq!(serde_json::from_str::<FileChange>(&text).unwrap(), local);
    let remote = FileChange {
        machine: MachineName::new("dev-a"),
        ..local
    };
    let text = serde_json::to_string(&remote).unwrap();
    assert_eq!(
        text,
        r#"{"path":"a.txt","additions":1,"deletions":0,"machine":"dev-a"}"#
    );
    assert_eq!(serde_json::from_str::<FileChange>(&text).unwrap(), remote);
}
