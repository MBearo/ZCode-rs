//! File changes over the Node database (spec rust-m11-node-storage §5.5): a
//! file-mutating tool result stores Node's workspace checkpoint artifact and
//! entry, which a resumed session reads back.
use super::harness;
use serde_json::Value;
use zcode_cli_state::node::{artifacts, entries, resume};

#[tokio::test]
async fn file_writes_are_stored_as_nodes_workspace_checkpoints() {
    let mut h = harness::start(Some("files"), None).await;
    let session = h.create("c1", "write it").await;
    let conn = h.settled(&session, 1).await;
    let stored = entries::list(&conn, &session, Some("runtime/workspace_checkpoint")).unwrap();
    assert_eq!(stored.len(), 1);
    let entry = &stored[0];
    let payload = &entry.data["payload"];
    assert_eq!(
        entry.id,
        format!(
            "workspace-checkpoint:{}",
            entry.data["eventId"].as_str().unwrap()
        )
    );
    assert!(
        payload["checkpointId"]
            .as_str()
            .unwrap()
            .starts_with("checkpoint_")
    );
    assert_eq!(payload["messageId"], payload["targetMessageId"]);
    assert_eq!(payload["scope"], "workspace");
    assert_eq!(payload["snapshotRef"], payload["diffRef"]);
    let uri = payload["snapshotRef"].as_str().unwrap();
    let root = h.root.join("cli/artifacts");
    let artifact: Value = serde_json::from_str(&artifacts::read(&root, uri).unwrap()).unwrap();
    assert_eq!(artifact["kind"], "workspace_file_before_change");
    assert_eq!(artifact["toolName"], "Write");
    assert_eq!(artifact["files"][0]["afterContent"], "x");
    assert_eq!(artifact["files"][0]["existedBefore"], false);
    let resumed = resume::resume(&conn, &session, &|u| artifacts::read(&root, u), None)
        .unwrap()
        .unwrap();
    let imported = &resumed.checkpoints;
    assert_eq!(imported.len(), 1);
    assert_eq!(
        (imported[0].tool.as_str(), imported[0].after.as_str()),
        ("Write", "x")
    );
    assert!(imported[0].before.is_none() && !imported[0].restored);
    harness::dump(&h, &conn, &session);

    // 重启后：Node 冷用量种子——上下文用量取最后一个 assistant 的 tokens，窗口取模型。
    drop(conn);
    let mut h = harness::restart(&h).await;
    h.rows(10, &session).await;
    let read = h
        .request(
            11,
            "session/read",
            serde_json::json!({"sessionId": session}),
        )
        .await;
    let projection = &read["projection"];
    assert_eq!(projection["contextUsed"], 105, "{read:#}");
    assert!(
        projection["contextWindow"]
            .as_u64()
            .is_some_and(|max| max > 0)
    );
}
