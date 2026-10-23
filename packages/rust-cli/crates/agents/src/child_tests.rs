//! 子会话的文件 checkpoint（session-lifecycle.md §4.2、agents.md §4.1）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    AgentId, ArtifactId, EnvironmentId, MessageId, SessionId, ToolUseId, TraceId,
};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::orchestration::{QueryEvent, RunSink};
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactRef, SessionKind, SessionMeta, SessionStore,
};
use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds};
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::json;

use super::ChildSink;
use crate::live::Live;

#[tokio::test]
async fn subagent_writes_settle_checkpoints_in_the_child_session() {
    let store = Arc::new(MemoryStore::default());
    let child = SessionId::new("sess_subagent_agent_1");
    store
        .create(SessionMeta {
            id: child.clone(),
            kind: SessionKind::Subagent,
            parent: Some(SessionId::new("sess_parent")),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            title: "edit".to_owned(),
            trace: TraceId::new("trace"),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let sink = ChildSink::new(
        store.clone(),
        Arc::new(SequentialIds::default()),
        Arc::new(Live::default()),
        AgentId::new("agent_1"),
        child.clone(),
        MessageId::new("prompt_1"),
        None,
    );
    let artifact = ArtifactRef {
        session: child.clone(),
        id: ArtifactId::new("checkpoint-tool-result-1.json"),
        purpose: ArtifactPurpose::Checkpoint,
        media_type: "application/json".to_owned(),
        size: 10,
    };
    sink.emit(QueryEvent::ToolFinished {
        tool_use: ToolUseId::new("toolu_1"),
        output: ToolOutput {
            content: Vec::new(),
            is_error: false,
            display: json!({"checkpoint": {"artifact": artifact, "path": "/Users/dev/work/a.txt", "additions": 1, "deletions": 0}}),
        },
    })
    .await;
    sink.commit_message(Message::User(UserMessage {
        id: MessageId::new("result_1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("toolu_1"),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("assistant_1"),
        },
    }))
    .await
    .unwrap();
    let loaded = store.load(&child).await.unwrap();
    assert_eq!(loaded.checkpoints.len(), 1);
    let checkpoint = &loaded.checkpoints[0];
    assert_eq!(checkpoint.prompt, MessageId::new("prompt_1"));
    assert_eq!(checkpoint.assistant, MessageId::new("assistant_1"));
    assert_eq!(checkpoint.tool_use, ToolUseId::new("toolu_1"));
    assert_eq!(checkpoint.snapshot, artifact);
}

#[tokio::test]
async fn subagent_calls_are_committed_with_their_messages_in_the_child_session() {
    use rust_cli_kernel_api::message::AssistantMessage;
    use rust_cli_kernel_api::metering::{CallStatus, Metered, ToolCall};
    use rust_cli_kernel_api::store::Fact;

    let store = Arc::new(MemoryStore::default());
    let child = SessionId::new("sess_subagent_agent_2");
    store
        .create(SessionMeta {
            id: child.clone(),
            kind: SessionKind::Subagent,
            parent: Some(SessionId::new("sess_parent")),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/work"),
            },
            title: "look".to_owned(),
            trace: TraceId::new("trace"),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let sink = ChildSink::new(
        store.clone(),
        Arc::new(SequentialIds::default()),
        Arc::new(Live::default()),
        AgentId::new("agent_2"),
        child.clone(),
        MessageId::new("prompt_1"),
        None,
    );
    let call = ToolCall {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Read".to_owned(),
        started_at_ms: 1,
        completed_at_ms: 2,
        status: CallStatus::Completed,
        output_bytes: 2,
    };
    let assistant = Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant_1"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    });
    sink.commit_metered(Some(assistant), Metered::Tool(call.clone()))
        .await
        .unwrap();
    sink.commit_metered(None, Metered::Tool(call.clone()))
        .await
        .unwrap();
    let facts = store.facts(&child);
    assert!(matches!(facts[0], Fact::MessageAppended { .. }));
    assert_eq!(facts[1], Fact::ToolCalled { call: call.clone() });
    assert_eq!(facts[2], Fact::ToolCalled { call });
    // 同批提交的 assistant 仍记进子代理的结果（最后一条 assistant）。
    assert_eq!(
        sink.outcome().0.map(|a| a.id),
        Some(MessageId::new("assistant_1"))
    );
}
