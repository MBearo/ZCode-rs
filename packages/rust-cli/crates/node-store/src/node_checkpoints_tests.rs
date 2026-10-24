//! Node 写的 checkpoint（修复线 F12，node-store.md §5 第 7 步、§4.4）：没有 `data.rustCli` 的
//! `runtime/workspace_checkpoint` 按 `snapshotRef` 与 Node 的产物文件名补出引用，冷读取给出，`get` 读得到。
//! 以前一律不给出：续写 App 改过文件的会话时，rewind 选择器里 Node 回合没有改动、不能恢复代码。
#![expect(
    clippy::disallowed_methods,
    reason = "测试直接写 Node 命名的产物文件（模拟 Node 进程）"
)]
use rust_cli_kernel_api::ids::{ArtifactId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactRef, ArtifactStore, SessionStore, WorkspaceCheckpoint,
};
use serde_json::json;

use crate::tests::{TestStore, batch, started, user};

const ARTIFACT: &str = "tool-result-0b7ea269-b8a7-4850-acf4-7830e6ba7548";

/// Node `persistWorkspaceCheckpointEntry` 的一条记录（另一条连接写，模拟 Node 进程），与 Node
/// `NodeToolArtifactStore` 命名的产物文件 `<toolCallId>-<artifactId>.json`。
fn node_checkpoint(t: &TestStore, sequence: u64, file: Option<&str>) {
    let uri = format!("zcode-artifact://sess_1/{ARTIFACT}");
    let data = json!({
        "eventId": "evt_node",
        "payload": {
            "checkpointId": "checkpoint_node", "messageId": "msg_u1", "targetMessageId": "msg_u1",
            "toolMessageId": "msg_a1", "scope": "workspace", "snapshotRef": uri, "diffRef": uri,
            "fileCount": 1,
        },
        "sequenceNumber": sequence,
        "traceId": "trace_node",
        "turnId": "turn_node",
    });
    t.connection()
        .execute(
            "insert into session_entry (id, session_id, type, time_created, time_updated, data) values ('workspace-checkpoint:evt_node', 'sess_1', 'runtime/workspace_checkpoint', 1, 1, ?)",
            [data.to_string()],
        )
        .unwrap();
    if let Some(content) = file {
        let dir = t.dir.path().join("cli/artifacts/sess_1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("toolu_node_1-{ARTIFACT}.json")), content).unwrap();
    }
}

fn expected(size: u64) -> WorkspaceCheckpoint {
    WorkspaceCheckpoint {
        machine: None,
        id: "checkpoint_node".into(),
        prompt: MessageId::new("msg_u1"),
        assistant: MessageId::new("msg_a1"),
        tool_use: ToolUseId::new("toolu_node_1"),
        snapshot: ArtifactRef {
            session: SessionId::new("sess_1"),
            id: ArtifactId::new(ARTIFACT),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".into(),
            size,
        },
    }
}

#[tokio::test]
async fn node_checkpoints_are_loaded_and_their_artifacts_read() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let sid = SessionId::new("sess_1");
    t.store
        .commit(&sid, batch(None, vec![user("msg_u1", "write it")]))
        .await
        .unwrap();
    let content = r#"{"files":[],"kind":"workspace_file_before_change","toolCallId":"toolu_node_1","version":1}"#;
    node_checkpoint(&t, 29, Some(content));
    let loaded = t.store.load(&sid).await.unwrap();
    assert_eq!(loaded.checkpoints, [expected(content.len() as u64)]);
    assert_eq!(
        t.store.get(&loaded.checkpoints[0].snapshot).await.unwrap(),
        content.as_bytes()
    );
}

/// 产物文件不在（例如另一台机器上的库）：不能回退，不给出；Node 撤销过的回合同样不给出。
#[tokio::test]
async fn node_checkpoints_without_a_file_or_already_rewound_are_left_out() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let sid = SessionId::new("sess_1");
    t.store
        .commit(&sid, batch(None, vec![user("msg_u1", "write it")]))
        .await
        .unwrap();
    node_checkpoint(&t, 29, None);
    assert!(t.store.load(&sid).await.unwrap().checkpoints.is_empty());

    let rewind = json!({
        "eventId": "evt_rewind",
        "payload": {"rewindId": "rewind_node", "scope": "workspace", "strategy": "active_chain",
                    "targetMessageId": "msg_u1", "targetCheckpointId": "checkpoint_node",
                    "restoredSnapshotRef": format!("zcode-artifact://sess_1/{ARTIFACT}"),
                    "reason": "file_summary_rewind"},
        "sequenceNumber": 32,
        "traceId": "trace_node",
    });
    let dir = t.dir.path().join("cli/artifacts/sess_1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("toolu_node_1-{ARTIFACT}.json")), "{}").unwrap();
    assert_eq!(t.store.load(&sid).await.unwrap().checkpoints, [expected(2)]);
    t.connection()
        .execute(
            "insert into session_entry (id, session_id, type, time_created, time_updated, data) values ('workspace-file-rewind:rewind_node', 'sess_1', 'runtime/workspace_file_rewind', 2, 2, ?)",
            [rewind.to_string()],
        )
        .unwrap();
    assert!(t.store.load(&sid).await.unwrap().checkpoints.is_empty());
}
