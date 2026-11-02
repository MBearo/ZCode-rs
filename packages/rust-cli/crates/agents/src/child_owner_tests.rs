//! 前台子代理把会话级请求转给父会话；后台子代理没有父调用（docs/specs/rust-cli/tools-more.md §10.2）。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId};
use rust_cli_kernel_api::orchestration::{Interactions, OrchestrationError};
use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds};
use tokio::sync::mpsc;

use super::*;

/// 父会话：顶层会话是 `sess_root`，记下问到的会话、收到的工作目录与撤回的询问。
#[derive(Default)]
struct Root(std::sync::Mutex<Vec<String>>);

impl Root {
    fn note(&self, line: String) {
        self.0.lock_unpoisoned().push(line);
    }
}

/// 只记下撤回（询问没有人回答）。
#[async_trait]
impl rust_cli_kernel_api::testing::AskOverride for Root {
    async fn cancel_ask(&self, tool_use: &rust_cli_kernel_api::ids::ToolUseId) {
        self.note(format!("cancel {tool_use}"));
    }
}

#[async_trait]
impl rust_cli_kernel_api::testing::OpsOverride for Root {
    async fn owner_session(&self, session: &SessionId) -> SessionId {
        self.note(format!("owner of {session}"));
        SessionId::new("sess_root")
    }

    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        self.note(format!("workspace {}", root.as_str()));
        Ok(())
    }
}

fn sink(parent: Option<Parent>) -> ChildSink {
    ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        Arc::default(),
        AgentId::new("agent_1"),
        SessionId::new("sess_child"),
        MessageId::new("prompt_1"),
        parent,
    )
}

#[tokio::test]
async fn foreground_children_forward_to_the_parent_session() {
    let root = Arc::new(Root::default());
    let (progress, _rx) = mpsc::channel(1);
    let child = sink(Some(Parent {
        session: SessionId::new("sess_parent"),
        asks: root.clone(),
        ops: root.clone(),
        progress,
    }));
    let own = SessionId::new("sess_child");
    assert_eq!(child.owner_session(&own).await, SessionId::new("sess_root"));
    child
        .set_workspace(EnvPath::new("/Users/dev/repo/.zcode/worktrees/a"))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    // 被取消的询问撤回也转给父会话（R1-5）。
    child
        .cancel_ask(&rust_cli_kernel_api::ids::ToolUseId::new("toolu_1"))
        .await;
    assert_eq!(
        *root.0.lock_unpoisoned(),
        [
            "owner of sess_parent",
            "workspace /Users/dev/repo/.zcode/worktrees/a",
            "cancel toolu_1"
        ]
    );
}

#[tokio::test]
async fn background_children_own_their_session() {
    let child = sink(None);
    let own = SessionId::new("sess_child");
    assert_eq!(child.owner_session(&own).await, own);
    assert!(
        child
            .set_workspace(EnvPath::new("/Users/dev"))
            .await
            .is_err()
    );
}
