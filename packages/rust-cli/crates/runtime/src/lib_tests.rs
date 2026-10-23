use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, KernelEvent, TurnOutcome,
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
        root: EnvPath::new("/work"),
    }
}

#[tokio::test]
async fn a_created_session_runs_a_turn_and_reloads_after_restart() {
    let store = Arc::new(MemoryStore::default());
    let first = kernel(
        store.clone(),
        Arc::new(ScriptedModel::new(vec![text_response(&["hi"])])),
    );
    let id = first
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let session = first.open_session(&id).await.unwrap();
    let again = first.open_session(&id).await.unwrap();
    assert!(Arc::ptr_eq(&session, &again));
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hello".to_owned(),
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
        if let KernelEvent::TurnFinished { outcome, .. } = event.event {
            assert_eq!(outcome, TurnOutcome::Success);
            break;
        }
    }
    drop(first);

    let second = kernel(store, Arc::new(ScriptedModel::new(Vec::new())));
    let reopened = second.open_session(&id).await.unwrap();
    // 一问一答之外，还有上下文这一步宣布的 attachment（模型身份、agent 清单（有 Agent 工具）、剩余
    // token、日期；内置工具里有 Bash，所以也宣布署名），它们与对话一起落库、resume 后重放。
    let messages = reopened.view().await.unwrap().messages;
    let dialogue = messages
        .iter()
        .filter(|message| matches!(message, Message::User(_) | Message::Assistant(_)))
        .count();
    assert_eq!(dialogue, 2);
    let sources: Vec<&str> = messages
        .iter()
        .filter_map(|message| match message {
            Message::Attachment(attachment) => Some(attachment.source.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sources,
        [
            "model",
            "agent_listing_delta",
            "total_tokens_reminder",
            "date",
            "remote_session_change"
        ]
    );
}

/// 复现（R1-1）：以前关闭后会话还留在 `sessions` 表里（actor 活着），再打开拿到的是旧 actor——不跑
/// SessionStart(resume)，按会话的表也不释放。关闭之后它离开内核，再打开是新的 actor。
#[tokio::test]
async fn a_closed_session_leaves_the_kernel_and_reopens_with_a_new_actor() {
    use rust_cli_kernel_api::session_api::{EndReason, StartSource};
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store, Arc::new(ScriptedModel::new(Vec::new())));
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let first = kernel.open_session(&id).await.unwrap();
    first.close(EndReason::Resume).await;
    assert!(
        !kernel.sessions().contains_key(&id),
        "a closed session must leave the kernel"
    );
    assert!(first.view().await.is_err(), "the old actor has stopped");
    let again = kernel
        .open_session_as(&id, StartSource::Resume)
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &again));
    again.view().await.unwrap();
}
