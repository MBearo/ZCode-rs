//! ← 会话面板的数据（tui-panels.md §2.1）：库里的会话合并本进程打开着的会话的状态。
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::panels::SessionActivity;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, selection,
    text_response,
};

use super::*;

fn kernel(store: Arc<MemoryStore>, model: Arc<ScriptedModel>) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model,
            env: Arc::new(UnusedEnvironment),
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

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/work"),
    }
}

fn create() -> CreateSession {
    CreateSession {
        id: None,
        workspace: workspace(),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
    }
}

#[tokio::test]
async fn open_sessions_carry_their_state_and_stored_ones_are_inactive() {
    let store = Arc::new(MemoryStore::default());
    // 上一个进程建的会话：只在库里。
    let earlier = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())));
    let stored = earlier
        .create_session(CreateSession {
            id: Some(SessionId::new("sess_earlier")),
            ..create()
        })
        .await
        .unwrap();
    drop(earlier);

    let current = kernel(
        store.clone(),
        Arc::new(ScriptedModel::new(vec![text_response(&["all done"])])),
    );
    let id = current.create_session(create()).await.unwrap();
    let session = current.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "finish it".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let filter = SessionFilter {
        workspace: workspace(),
        limit: 50,
    };
    let sessions = current.workspace_sessions(filter.clone()).await.unwrap();
    let find = |wanted: &SessionId| {
        sessions
            .iter()
            .find(|session| &session.summary.id == wanted)
            .unwrap()
            .clone()
    };
    let open = find(&id);
    assert_eq!(open.activity, Some(SessionActivity::Ready));
    assert_eq!(open.last_reply.as_deref(), Some("all done"));
    let old = find(&stored);
    assert_eq!(old.activity, None);
    assert_eq!(old.last_reply, None);

    // 别的工作区：打开着的会话不属于它，不带实时状态（库里的列表按工作区过滤是 node-store 的事）。
    let elsewhere = SessionFilter {
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/other"),
        },
        limit: 50,
    };
    let other = current.workspace_sessions(elsewhere).await.unwrap();
    assert!(other.iter().all(|session| session.activity.is_none()));
}
