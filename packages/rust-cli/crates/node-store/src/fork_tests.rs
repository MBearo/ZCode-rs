//! Node `/fork` 的新会话（session-lifecycle.md §6.5.2、node-store.md §4.8）：会话行、复制的范围、提示与时间线、
//! 冷读取、找不到目标时整批回滚。
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_rewind::{RestoreAction, RestoredFile};
use rust_cli_kernel_api::ids::{ArtifactId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactRef, Fact, RevertCursor, SessionFork, SessionStore,
    WorkspaceCheckpoint,
};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, selection, started, text, user};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn checkpoint() -> WorkspaceCheckpoint {
    WorkspaceCheckpoint {
        id: "checkpoint_1".into(),
        prompt: MessageId::new("msg_3_u2"),
        assistant: MessageId::new("msg_4_a2"),
        tool_use: ToolUseId::new("toolu_1"),
        snapshot: ArtifactRef {
            session: sid("sess_1"),
            id: ArtifactId::new("tool-result-cp"),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".into(),
            size: 12,
        },
    }
}

fn request(through: &str, restored: bool) -> SessionFork {
    SessionFork {
        parent: sid("sess_1"),
        child: sid("sess_2"),
        created_at_ms: 36_000,
        through: MessageId::new(through),
        checkpoint: restored.then(checkpoint),
        restored: if restored {
            vec![RestoredFile {
                path: EnvPath::new("/w/a.txt"),
                action: RestoreAction::Restore,
            }]
        } else {
            Vec::new()
        },
        selection: selection(),
        mode: PermissionMode::AcceptEdits,
        plan: false,
    }
}

async fn three_turns(t: &TestStore) {
    started(t, "sess_1").await;
    for fact in [
        user("msg_1_u1", "first"),
        assistant("msg_2_a1", vec![text("one")], None),
        user("msg_3_u2", "second"),
        assistant("msg_4_a2", vec![text("two")], None),
        user("msg_5_u3", "third"),
    ] {
        t.store
            .commit(&sid("sess_1"), batch(None, vec![fact]))
            .await
            .unwrap();
    }
}

fn data(t: &TestStore, table: &str, session: &str) -> Vec<Value> {
    t.rows(&format!(
        "select data from {table} where session_id = '{session}' order by rowid"
    ))
    .into_iter()
    .map(|row| serde_json::from_str(row[0].as_str().unwrap()).unwrap())
    .collect()
}

#[tokio::test]
async fn the_fork_copies_up_to_the_checkpoint_turn_and_adds_the_notice() {
    let t = TestStore::new();
    three_turns(&t).await;
    let receipt = t.store.fork(request("msg_3_u2", true)).await.unwrap();
    assert_eq!(receipt.copied, 3);
    // 会话行：Node `buildForkedSessionInput`。
    let row = &t.rows(
        "select parent_id, task_type, slug, title, title_source, directory, version, permission
         from session where id = 'sess_2'",
    )[0];
    assert_eq!(row[0], json!("sess_1"));
    assert_eq!(row[1], json!("fork"));
    assert_eq!(row[2], json!("sess_1-fork-rs0"));
    assert_eq!(row[3], json!("Fork of first"));
    assert_eq!(row[4], json!("generated"));
    assert_eq!(row[5], json!("/w"));
    assert_eq!(
        serde_json::from_str::<Value>(row[7].as_str().unwrap()).unwrap(),
        json!({"mode": "edit"})
    );
    let messages = data(&t, "message", "sess_2");
    // 三条复制的消息 + 时间线 + 提示。
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages[0]["metadata"]["forkOrigin"],
        json!({"sessionId": "sess_1", "messageId": "msg_1_u1"})
    );
    assert_eq!(messages[3]["semantics"]["kind"], "timeline_event");
    assert_eq!(messages[4]["source"], "fork");
    assert_eq!(messages[4]["semantics"]["kind"], "fork_notice");
    let ids: Vec<String> = t
        .rows("select id from message where session_id = 'sess_2' order by sequence")
        .into_iter()
        .map(|row| row[0].as_str().unwrap().to_owned())
        .collect();
    // assistant 的 parentID 换成复制后的 user 消息。
    assert_eq!(messages[1]["parentID"], json!(ids[0]));
    assert!(ids.iter().all(|id| !id.starts_with("msg_1_")));
    let parts = data(&t, "part", "sess_2");
    let timeline = parts.iter().find(|p| p["type"] == "timeline").unwrap();
    assert_eq!(timeline["timelineType"], "session_fork");
    assert_eq!(timeline["anchorMessageId"], json!(ids[2]));
    assert_eq!(timeline["targetCheckpointId"], "checkpoint_1");
    assert_eq!(timeline["restoredFileCount"], 1);
    let notice = parts
        .iter()
        .find(|p| p["metadata"]["source"] == "fork")
        .unwrap();
    assert_eq!(
        notice["text"],
        format!(
            "This session was forked from a previous session checkpoint.\nparentSessionId: sess_1\ncheckpointId: checkpoint_1\ntargetMessageId: msg_3_u2\nrestoredSnapshotRef: zcode-artifact://sess_1/tool-result-cp\nrestoredFiles: 1\nrestore /w/a.txt\n{}",
            super::CONTINUE
        )
    );
    assert_eq!(
        notice["metadata"]["runtimeMessage"],
        json!({"source": "rewind_notice"})
    );
    // 冷读取：复制的历史，时间线跳过，提示是 `fork` 来源的 attachment。
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    assert_eq!(loaded.mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(loaded.meta.title, "Fork of first");
    let kinds: Vec<String> = loaded
        .messages
        .iter()
        .map(|message| match message {
            Message::User(_) => "user".to_owned(),
            Message::Assistant(_) => "assistant".to_owned(),
            Message::Attachment(attachment) => attachment.source.clone(),
            Message::System(_) => "system".to_owned(),
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "user", "fork"]);
    let Message::Attachment(fork) = &loaded.messages[3] else {
        unreachable!()
    };
    assert!(
        fork.text
            .starts_with("<system-reminder>\nThis session was forked")
    );
    // 原会话不变。
    assert_eq!(data(&t, "message", "sess_1").len(), 5);
}

#[tokio::test]
async fn a_conversation_only_fork_follows_the_revert_cursor() {
    let t = TestStore::new();
    three_turns(&t).await;
    // 回退到第二条输入之前：fork 源分支里没有它。
    let cursor = Fact::RevertSet {
        cursor: RevertCursor {
            target: MessageId::new("msg_3_u2"),
            kept: vec![MessageId::new("msg_1_u1"), MessageId::new("msg_2_a1")],
            cut_after: MessageId::new("msg_5_u3"),
            generation: 1,
        },
    };
    t.store
        .commit(&sid("sess_1"), batch(None, vec![cursor]))
        .await
        .unwrap();
    let error = t.store.fork(request("msg_3_u2", false)).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Checkpoint message not found in session store: msg_3_u2"),
        "{error}"
    );
    // 整批回滚：没有留下会话行。
    assert!(
        t.rows("select id from session where id = 'sess_2'")
            .is_empty()
    );
    let receipt = t.store.fork(request("msg_2_a1", false)).await.unwrap();
    assert_eq!(receipt.copied, 2);
    let parts = data(&t, "part", "sess_2");
    let notice = parts
        .iter()
        .find(|p| p["metadata"]["source"] == "fork")
        .unwrap();
    assert_eq!(
        notice["text"],
        format!(
            "This session was forked from a previous session message.\nparentSessionId: sess_1\ntargetMessageId: msg_2_a1\nNo workspace checkpoint was restored for this fork.\n{}",
            super::CONTINUE
        )
    );
    assert!(
        notice["metadata"]["forkContext"]
            .get("targetCheckpointId")
            .is_none()
    );
}

#[tokio::test]
async fn the_goal_state_is_copied_with_its_verifications() {
    let t = TestStore::new();
    three_turns(&t).await;
    {
        let conn = t.connection();
        conn.execute(
            "insert into session_target (session_id, target_id, objective, summary_title, status,
               token_budget, tokens_used, time_used_seconds, active_input_id, time_created, time_updated)
             values ('sess_1', 'target_1', 'ship it', null, 'complete', null, 5, 2, 'input_9', 1, 2)",
            [],
        )
        .unwrap();
        for (id, anchor) in [("v1", "msg_2_a1"), ("v2", "msg_4_a2"), ("v3", "msg_9_gone")] {
            let data = json!({"eventId": id, "payload": {"targetId": "target_1",
                "anchorAssistantMessageId": anchor, "anchorTurnId": "turn_1",
                "status": "completed", "verification": {"passed": false}}});
            conn.execute(
                "insert into session_entry (id, session_id, type, time_created, time_updated, data)
                 values (?, 'sess_1', 'target_completion_verification', 1, 1, ?)",
                rusqlite::params![id, data.to_string()],
            )
            .unwrap();
        }
    }
    t.store.fork(request("msg_3_u2", false)).await.unwrap();
    let target = &t.rows(
        "select target_id, objective, status, tokens_used, active_input_id from session_target
         where session_id = 'sess_2'",
    )[0];
    // 复制的验证没有通过，父目标是 complete：分出的目标回到 active（Node 的推导）。
    assert_eq!(
        target,
        &vec![
            json!("target_1"),
            json!("ship it"),
            json!("active"),
            json!(5),
            Value::Null
        ]
    );
    let copied = t.rows(
        "select data from session_entry where session_id = 'sess_2'
           and type = 'target_completion_verification'",
    );
    // 锚定 msg_4_a2（复制范围外）与已不存在的消息的验证都跳过。
    assert_eq!(copied.len(), 1);
    let payload: Value = serde_json::from_str(copied[0][0].as_str().unwrap()).unwrap();
    assert_eq!(payload["payload"]["originAnchorTurnId"], "turn_1");
    assert!(payload["payload"].get("anchorTurnId").is_none());
    assert_ne!(payload["payload"]["anchorAssistantMessageId"], "msg_2_a1");
}
