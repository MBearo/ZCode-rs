//! ← 项目页与 `/resume` 页的内核部分（修复线 F29，tui-panels.md §2、tui-manage.md §10）：查询合并实时状态与分支、
//! 改名、删除、只读快照。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::panels::SessionActivity;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelError,
};
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment,
    environments, selection, text_response,
};

use super::*;

fn kernel<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    store: Arc<MemoryStore>,
    env: Arc<E>,
) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(vec![text_response(&["done"])])),
            environments: environments(env),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    )
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

fn create(id: &str, root: &str) -> CreateSession {
    CreateSession {
        id: Some(SessionId::new(id)),
        workspace: workspace(root),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
    }
}

fn all() -> SessionQuery {
    SessionQuery {
        environment: EnvironmentId::new("local"),
        root: None,
        order: SessionOrder::Updated,
        limit: 200,
    }
}

/// 登记处：别的进程在跑或等你的会话。
struct Registry(Vec<rust_cli_kernel_api::peers::PeerRecord>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::peers::PeerMessaging for Registry {
    async fn identity(&self) -> Option<rust_cli_kernel_api::peers::PeerIdentity> {
        None
    }
    async fn peers(&self) -> Vec<rust_cli_kernel_api::peers::PeerRecord> {
        self.0.clone()
    }
    async fn send(
        &self,
        _message: rust_cli_kernel_api::peers::PeerOutbound,
    ) -> Result<(), rust_cli_kernel_api::peers::PeerSendError> {
        Ok(())
    }
    async fn receipt(
        &self,
        _to: rust_cli_kernel_api::peers::PeerAddress,
        _msg_id: String,
        _status: rust_cli_kernel_api::peers::ReceiptStatus,
    ) {
    }
    async fn bind(&self, _session: &SessionId) {}
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(
        &self,
        _session: &SessionId,
    ) -> futures_util::stream::BoxStream<'static, rust_cli_kernel_api::peers::PeerSignal> {
        Box::pin(futures_util::stream::pending())
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn set_status(
        &self,
        _session: &SessionId,
        _status: rust_cli_kernel_api::peers::PeerStatus,
    ) {
    }
}

/// 别的进程的记录合进行：状态、时刻、现场与地址都来自登记（tui-cockpit.md §1）。
#[tokio::test]
async fn sessions_live_in_another_process_carry_the_registry() {
    use rust_cli_kernel_api::peers::{PeerAddress, PeerPresence, PeerRecord, PeerStatus};
    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(UnusedEnvironment);
    let earlier = kernel(store.clone(), env.clone());
    earlier
        .create_session(create("elsewhere", "/w/app"))
        .await
        .unwrap();
    earlier
        .create_session(create("closed", "/w/app"))
        .await
        .unwrap();
    let presence = PeerPresence {
        now: Some("Bash  pnpm test".to_owned()),
        ..PeerPresence::default()
    };
    let record = PeerRecord {
        pid: 42,
        name: "app-1".to_owned(),
        address: PeerAddress::from_socket("/peers/42.sock"),
        kind: "interactive".to_owned(),
        status: Some(PeerStatus::Waiting),
        started_at_ms: 0,
        session: Some(SessionId::new("elsewhere")),
        status_since_ms: Some(700),
        presence: presence.clone(),
        tmux: None,
    };
    let kernel = LocalKernel::with_extensions(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
            environments: environments(env),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection().into(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
        crate::ContextConfig::new(crate::Surface::Print),
        crate::CompactConfig::default(),
        Arc::new(rust_cli_agents::MemoryTaskLists::default()),
        crate::Extensions {
            peers: rust_cli_session::PeerDeps {
                port: Arc::new(Registry(vec![record])),
                hold_timeout_ms: 1_000,
                receipts_wake: false,
            },
            ..crate::Extensions::none()
        },
    );
    let found = kernel.project_sessions(all()).await.unwrap().sessions;
    let of = |id: &str| {
        found
            .iter()
            .find(|s| s.record.id.as_str() == id)
            .unwrap_or_else(|| panic!("no {id}"))
    };
    let live = of("elsewhere");
    assert_eq!(live.activity, Some(SessionActivity::NeedsInput));
    let joined = live.presence.as_ref().unwrap();
    assert!(!joined.here);
    assert_eq!(joined.since_ms, Some(700));
    assert_eq!(joined.presence, presence);
    assert_eq!(joined.address, PeerAddress::from_socket("/peers/42.sock"));
    assert_eq!(of("closed").activity, None);
    assert!(of("closed").presence.is_none());
}

/// 两个目录的会话：打开着的有状态，只在库里的没有；每个目录的分支读一次 `.git/HEAD`。
#[tokio::test]
async fn sessions_of_every_project_carry_state_and_branch() {
    let store = Arc::new(MemoryStore::default());
    let env = Arc::new(
        MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
            cwd: EnvPath::new("/w/app"),
            platform: "darwin".to_owned(),
            os_version: String::new(),
            shell: "zsh".to_owned(),
            is_git_repo: true,
            home: None,
            uid: None,
        })
        .with_file("/w/app/.git/HEAD", "ref: refs/heads/main\n")
        // worktree：`.git` 是文件，指向主仓库里的 gitdir。
        .with_file("/w/lib/.git", "gitdir: ../main/.git/worktrees/lib\n")
        .with_file(
            "/w/main/.git/worktrees/lib/HEAD",
            "ref: refs/heads/feature/x\n",
        ),
    );
    let earlier = kernel(store.clone(), env.clone());
    earlier
        .create_session(create("sess_lib", "/w/lib"))
        .await
        .unwrap();
    drop(earlier);
    let current = kernel(store.clone(), env.clone());
    let id = current
        .create_session(create("sess_app", "/w/app"))
        .await
        .unwrap();
    let session = current.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hello".into(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    while let Some(event) = futures_util::StreamExt::next(&mut events).await {
        if matches!(
            event.event,
            rust_cli_kernel_api::session_api::KernelEvent::TurnFinished { .. }
        ) {
            break;
        }
    }
    let page = current.project_sessions(all()).await.unwrap();
    let find = |id: &str| {
        page.sessions
            .iter()
            .find(|s| s.record.id.as_str() == id)
            .unwrap()
            .clone()
    };
    let app = find("sess_app");
    assert_eq!(app.activity, Some(SessionActivity::Ready));
    assert_eq!(app.last_reply.as_deref(), Some("done"));
    assert_eq!(app.branch.as_deref(), Some("main"));
    assert_eq!(app.record.first_prompt.as_deref(), Some("hello"));
    let lib = find("sess_lib");
    assert_eq!(lib.activity, None);
    assert_eq!(lib.branch.as_deref(), Some("feature/x"));
    // 只看一个目录。
    let only = current
        .project_sessions(SessionQuery {
            root: Some(EnvPath::new("/w/lib")),
            ..all()
        })
        .await
        .unwrap();
    assert_eq!(only.sessions.len(), 1);
    assert_eq!(only.sessions[0].record.id.as_str(), "sess_lib");
}

/// 改名：打开着的经 actor（`TitleChanged`，视图与库都变），没打开的直接写库。删除：打开着的拒绝。
#[tokio::test]
async fn rename_goes_through_the_actor_when_open_and_delete_refuses_open_sessions() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(UnusedEnvironment));
    kernel
        .create_session(create("sess_closed", "/w"))
        .await
        .unwrap();
    let open = kernel
        .create_session(create("sess_open", "/w"))
        .await
        .unwrap();
    let handle = kernel.open_session(&open).await.unwrap();
    // 第一个会话关掉（离开内核）。
    let closed = SessionId::new("sess_closed");
    kernel
        .open_session(&closed)
        .await
        .unwrap()
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    kernel
        .rename_session(&open, "Open one".into())
        .await
        .unwrap();
    assert_eq!(handle.view().await.unwrap().meta.title, "Open one");
    assert_eq!(store.title(&open).as_deref(), Some("Open one"));
    kernel
        .rename_session(&closed, "Closed one".into())
        .await
        .unwrap();
    assert_eq!(store.title(&closed).as_deref(), Some("Closed one"));
    let refused = kernel.delete_session(&open).await;
    assert!(
        matches!(refused, Err(KernelError::Failed { .. })),
        "{refused:?}"
    );
    assert!(!store.is_deleted(&open));
    kernel.delete_session(&closed).await.unwrap();
    assert!(store.is_deleted(&closed));
    let missing = kernel.rename_session(&closed, "again".into()).await;
    assert!(
        matches!(missing, Err(KernelError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 只读快照：没打开的从库里冷加载，不打开会话（之后会话表里没有它）。
#[tokio::test]
async fn snapshots_do_not_open_sessions() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone(), Arc::new(UnusedEnvironment));
    let id = kernel.create_session(create("sess_s", "/w")).await.unwrap();
    kernel
        .open_session(&id)
        .await
        .unwrap()
        .close(rust_cli_kernel_api::session_api::EndReason::Clear)
        .await;
    assert!(!kernel.sessions().contains_key(&id));
    let view = kernel.session_snapshot(&id).await.unwrap();
    assert_eq!(view.meta.id, id);
    assert!(!view.running);
    assert!(
        !kernel.sessions().contains_key(&id),
        "a snapshot must not open the session"
    );
    let missing = kernel.session_snapshot(&SessionId::new("sess_x")).await;
    assert!(
        matches!(missing, Err(KernelError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 分支查找只把目录或文件的 `.git` 当仓库：FIFO 等别的种类继续往上（F58：以前把它当文件读，
/// 读不到就没有分支）。
#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_passed_over() {
    let env = MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
        cwd: EnvPath::new("/w/app"),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: true,
        home: None,
        uid: None,
    })
    .with_special("/w/app/.git")
    .with_file("/w/.git/HEAD", "ref: refs/heads/up\n");
    let found = branch(&env, &EnvironmentId::new("local"), &EnvPath::new("/w/app")).await;
    assert_eq!(found.as_deref(), Some("up"));
}
