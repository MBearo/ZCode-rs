//! 回退游标与 checkpoint 的 Node 记录（spec session-lifecycle.md §4.2、§5.3）。
use rust_cli_kernel_api::ids::{ArtifactId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactRef, Fact, RevertCursor, SessionStore, WorkspaceCheckpoint,
};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, started, text, user};

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

fn calls(id: &str, call: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new(call),
                name: "Write".into(),
                input: json!({"file_path": "/work/a.txt", "content": "x"}),
            }],
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    }
}

fn result(id: &str, owner: &str, call: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(call),
                content: vec![ToolResultContent::Text { text: "ok".into() }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(owner),
            },
        }),
    }
}

fn checkpoint(id: &str, prompt: &str, assistant: &str, call: &str) -> WorkspaceCheckpoint {
    WorkspaceCheckpoint {
        machine: None,
        id: id.into(),
        prompt: MessageId::new(prompt),
        assistant: MessageId::new(assistant),
        tool_use: ToolUseId::new(call),
        snapshot: ArtifactRef {
            session: sid(),
            id: ArtifactId::new(format!("tool-result-{id}")),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".into(),
            size: 12,
        },
    }
}

fn cursor(target: &str, generation: u64) -> Fact {
    // 内核给的 kept / cut_after 是内存里的 ID（含没有行的工具结果）；node-store 按库里的行重算。
    Fact::RevertSet {
        cursor: RevertCursor {
            target: MessageId::new(target),
            kept: vec![MessageId::new("msg_1_u1"), MessageId::new("msg_3_r1")],
            cut_after: MessageId::new("msg_3_r1"),
            generation,
        },
    }
}

async fn two_turns_with_a_write(t: &TestStore) {
    started(t, "sess_1").await;
    for fact in [
        user("msg_1_u1", "write it"),
        calls("msg_2_a1", "toolu_1"),
        result("msg_3_r1", "msg_2_a1", "toolu_1"),
        user("msg_4_u2", "again"),
        assistant("msg_5_a2", vec![text("done")], None),
    ] {
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
}

fn revert(t: &TestStore) -> Value {
    let rows = t.rows("select revert from session");
    serde_json::from_str(rows[0][0].as_str().unwrap()).unwrap()
}

/// Node `buildConversationRewindPlan`：保留前缀是活动分支里目标之前的行（工具结果没有行），
/// 切点是会话最后一行，代数在旧值上加一。
#[tokio::test]
async fn revert_cursors_are_computed_from_the_stored_rows() {
    let t = TestStore::new();
    two_turns_with_a_write(&t).await;
    t.store
        .commit(&sid(), batch(None, vec![cursor("msg_4_u2", 1)]))
        .await
        .unwrap();
    assert_eq!(
        revert(&t),
        json!({"keptMessageIDs": ["msg_1_u1", "msg_2_a1"], "branchCutAfterMessageID": "msg_5_a2",
               "branchGeneration": 1, "messageID": "msg_2_a1", "kind": "conversation_rewind",
               "scope": "conversation", "targetMessageID": "msg_4_u2"})
    );
    // 新分支上再回退到第一条：旧分支的行不回来，代数加一。
    for fact in [
        user("msg_6_u3", "other"),
        assistant("msg_7_a3", vec![text("ok")], None),
    ] {
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    t.store
        .commit(&sid(), batch(None, vec![cursor("msg_6_u3", 0)]))
        .await
        .unwrap();
    let value = revert(&t);
    assert_eq!(value["keptMessageIDs"], json!(["msg_1_u1", "msg_2_a1"]));
    assert_eq!(value["branchCutAfterMessageID"], "msg_7_a3");
    assert_eq!(value["branchGeneration"], 2);
    let loaded = t.store.load(&sid()).await.unwrap();
    let ids: Vec<&str> = loaded.messages.iter().map(|m| m.id().as_str()).collect();
    // 工具结果从 assistant 的 tool part 里拆回（消息 id 是结果消息的 id）。
    assert_eq!(ids, ["msg_1_u1", "msg_2_a1", "msg_3_r1"]);
    // 目标不在活动分支上时整批回滚。
    let error = t
        .store
        .commit(&sid(), batch(None, vec![cursor("msg_4_u2", 3)]))
        .await;
    assert!(error.is_err());
}

/// checkpoint 写成 Node `runtime/workspace_checkpoint`：payload 是 Node strict 字段，内核读回的引用在
/// payload 之外；文件回退为每个回合写一条 `runtime/workspace_file_rewind`，冷读取不再给出这些回合的
/// checkpoint。
#[tokio::test]
async fn checkpoints_use_node_entries_and_restores_hide_their_turns() {
    let t = TestStore::new();
    two_turns_with_a_write(&t).await;
    let first = checkpoint("checkpoint_a", "msg_1_u1", "msg_2_a1", "toolu_1");
    let second = checkpoint("checkpoint_b", "msg_4_u2", "msg_5_a2", "toolu_2");
    for one in [&first, &second] {
        let fact = Fact::CheckpointCreated {
            checkpoint: one.clone(),
        };
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let rows = t.rows(
        "select id, data from session_entry where type = 'runtime/workspace_checkpoint'
         order by time_created, rowid",
    );
    assert_eq!(rows.len(), 2);
    let data: Value = serde_json::from_str(rows[0][1].as_str().unwrap()).unwrap();
    let uri = "zcode-artifact://sess_1/tool-result-checkpoint_a";
    assert_eq!(
        data["payload"],
        json!({"checkpointId": "checkpoint_a", "messageId": "msg_1_u1",
               "targetMessageId": "msg_1_u1", "toolMessageId": "msg_2_a1", "scope": "workspace",
               "snapshotRef": uri, "diffRef": uri, "fileCount": 1})
    );
    assert_eq!(
        rows[0][0],
        Value::from(format!(
            "workspace-checkpoint:{}",
            data["eventId"].as_str().unwrap()
        ))
    );
    assert!(data["sequenceNumber"].as_u64().unwrap() > 0);
    assert_eq!(data["traceId"], "sess_1");
    assert!(data["turnId"].is_string());
    assert_eq!(data["rustCli"]["toolUse"], "toolu_1");
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.checkpoints, [first.clone(), second.clone()]);

    let fact = Fact::CheckpointsRestored {
        checkpoints: vec!["checkpoint_b".into()],
    };
    t.store
        .commit(&sid(), batch(None, vec![fact]))
        .await
        .unwrap();
    let rows =
        t.rows("select data from session_entry where type = 'runtime/workspace_file_rewind'");
    assert_eq!(rows.len(), 1);
    let data: Value = serde_json::from_str(rows[0][0].as_str().unwrap()).unwrap();
    let payload = &data["payload"];
    assert_eq!(payload["reason"], "file_summary_rewind");
    assert_eq!(payload["scope"], "workspace");
    assert_eq!(payload["targetMessageId"], "msg_4_u2");
    assert_eq!(payload["targetCheckpointId"], "checkpoint_b");
    assert_eq!(
        payload["restoredSnapshotRef"],
        "zcode-artifact://sess_1/tool-result-checkpoint_b"
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.checkpoints, [first]);
}

/// 序号按 Node 的事件序号空间：不小于库里已有的最大值（Node 写的 checkpoint），同一批的两个依次加一（修复线 F2；
/// 以前用本工程的提交序号，排在 Node 的 57 之前，两个还相同）。
#[tokio::test]
async fn checkpoint_sequences_follow_the_stored_node_events() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![user("msg_u1", "Write twice"), calls("msg_a1", "toolu_1")];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    t.connection()
        .execute(
            "insert into session_entry (id, session_id, type, time_created, time_updated, data) values ('workspace-checkpoint:evt_node', 'sess_1', 'runtime/workspace_checkpoint', 1, 1, ?)",
            [json!({"eventId": "evt_node", "payload": {}, "sequenceNumber": 57, "traceId": "sess_1"}).to_string()],
        )
        .unwrap();
    let facts = vec![
        result("msg_r1", "msg_a1", "toolu_1"),
        Fact::CheckpointCreated {
            checkpoint: checkpoint("checkpoint_1", "msg_u1", "msg_a1", "toolu_1"),
        },
        Fact::CheckpointCreated {
            checkpoint: checkpoint("checkpoint_2", "msg_u1", "msg_a1", "toolu_1"),
        },
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    assert_eq!(
        t.rows("select json_extract(data, '$.sequenceNumber') from session_entry where type = 'runtime/workspace_checkpoint' order by json_extract(data, '$.sequenceNumber')"),
        vec![vec![json!(57)], vec![json!(58)], vec![json!(59)]]
    );
}

/// 远端机器上的 checkpoint（R3，remote-exec.md §20.9）：写成本工程的 `rust-cli/remote_checkpoint`（Node 的类型里没有它），
/// 冷读取带回机器名；它的回退写 `rust-cli/remote_file_rewind`，不动 Node 的记录，本机同一回合的 checkpoint 仍可回退。
#[tokio::test]
async fn remote_checkpoints_use_rust_cli_entries() {
    let t = TestStore::new();
    two_turns_with_a_write(&t).await;
    let local = checkpoint("checkpoint_l", "msg_1_u1", "msg_2_a1", "toolu_1");
    let remote = WorkspaceCheckpoint {
        machine: Some("dev-a".into()),
        ..checkpoint("checkpoint_r", "msg_1_u1", "msg_2_a1", "toolu_1")
    };
    for one in [&local, &remote] {
        let fact = Fact::CheckpointCreated {
            checkpoint: one.clone(),
        };
        t.store
            .commit(&sid(), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let count = |kind: &str| {
        t.rows(&format!(
            "select count(*) from session_entry where type = '{kind}'"
        ))[0][0]
            .as_u64()
            .unwrap()
    };
    assert_eq!(count("runtime/workspace_checkpoint"), 1);
    assert_eq!(count("rust-cli/remote_checkpoint"), 1);
    let rows =
        t.rows("select id, data from session_entry where type = 'rust-cli/remote_checkpoint'");
    let data: Value = serde_json::from_str(rows[0][1].as_str().unwrap()).unwrap();
    assert_eq!(data["rustCli"]["machine"], "dev-a");
    // 序号在本工程的记录之间单独算。
    assert_eq!(data["sequenceNumber"], 1);
    assert!(
        rows[0][0]
            .as_str()
            .unwrap()
            .starts_with("rust-cli-remote-checkpoint:")
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.checkpoints, [local.clone(), remote.clone()]);

    // 只回退远端那个：写本工程的回退记录，本机的 checkpoint 还在。
    let fact = Fact::CheckpointsRestored {
        checkpoints: vec!["checkpoint_r".into()],
    };
    t.store
        .commit(&sid(), batch(None, vec![fact]))
        .await
        .unwrap();
    assert_eq!(count("runtime/workspace_file_rewind"), 0);
    assert_eq!(count("rust-cli/remote_file_rewind"), 1);
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.checkpoints, [local]);
}
