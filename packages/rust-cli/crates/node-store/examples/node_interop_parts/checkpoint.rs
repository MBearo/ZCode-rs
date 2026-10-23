//! 互操作的第五段会话：文件 checkpoint 与对话回退（docs/specs/rust-cli/session-lifecycle.md §4、§5）。
//!
//! 第一轮 Write 改 a.txt：工具结果与 checkpoint 一个事务提交，artifact 是 Node 的
//! `workspace_file_before_change`；第二轮之后回退到第二轮的输入（`session.revert`），活动分支只剩第一轮。
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactStore, WorkspaceCheckpoint};

use super::*;

pub(super) const CHECKPOINT_FILE: &str = "/w/interop/a.txt";

pub(super) async fn checkpointed(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let (u1, a1, a2, u2, a3) = (
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
    );
    store
        .commit(&id, batch(Some("cmd_k1"), vec![user(&u1, "Change a.txt")]))
        .await?;
    let input = json!({"file_path": CHECKPOINT_FILE, "content": "new\n"});
    let calls = reply(
        &a1,
        vec![call("toolu_k1", "Write", &input)],
        StopReason::ToolUse,
    );
    store.commit(&id, batch(None, vec![calls])).await?;
    let artifact = json!({
        "createdAt": "2026-10-17T00:00:00.000Z",
        "files": [{"afterContent": "new\n", "afterContentLength": 4, "beforeContent": "old\n",
            "existedBefore": true, "path": CHECKPOINT_FILE,
            "structuredPatch": [{"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                "lines": ["-old", "+new"]}]}],
        "kind": "workspace_file_before_change",
        "toolCallId": "toolu_k1",
        "toolName": "Write",
        "version": 1,
    });
    let snapshot = store
        .put(
            &id,
            ArtifactPurpose::Checkpoint,
            "application/json".into(),
            serde_json::to_vec_pretty(&artifact)?,
        )
        .await?;
    let checkpoint = WorkspaceCheckpoint {
        id: "checkpoint_interop".into(),
        prompt: u1.clone(),
        assistant: a1.clone(),
        tool_use: ToolUseId::new("toolu_k1"),
        snapshot,
    };
    let facts = vec![
        result(&a1, "toolu_k1", vec![said("written")], false),
        Fact::CheckpointCreated { checkpoint },
    ];
    store.commit(&id, batch(None, facts)).await?;
    store
        .commit(
            &id,
            batch(
                None,
                vec![reply(&a2, vec![text("Changed.")], StopReason::EndTurn)],
            ),
        )
        .await?;
    store
        .commit(
            &id,
            batch(Some("cmd_k2"), vec![user(&u2, "Undo that later")]),
        )
        .await?;
    store
        .commit(
            &id,
            batch(
                None,
                vec![reply(&a3, vec![text("Sure.")], StopReason::EndTurn)],
            ),
        )
        .await?;
    // 内核给的游标是内存里的 ID；node-store 按库里的行重算（工具结果没有行）。
    let cursor = RevertCursor {
        target: u2.clone(),
        kept: vec![u1, a1, a2],
        cut_after: a3,
        generation: 1,
    };
    store
        .commit(&id, batch(Some("cmd_k3"), vec![Fact::RevertSet { cursor }]))
        .await?;
    Ok(id)
}
