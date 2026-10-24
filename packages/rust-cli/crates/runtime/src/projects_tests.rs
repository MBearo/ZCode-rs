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
            selection: selection(),
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
            additional_directories: Vec::new(),
            home: None,
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
        additional_directories: Vec::new(),
        home: None,
    })
    .with_special("/w/app/.git")
    .with_file("/w/.git/HEAD", "ref: refs/heads/up\n");
    let found = branch(&env, &EnvironmentId::new("local"), &EnvPath::new("/w/app")).await;
    assert_eq!(found.as_deref(), Some("up"));
}
