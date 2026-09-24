//! Permission grants over the Node database: full access commits the
//! execution state and Node's receipt, which a restart reads back (spec
//! rust-m11-node-storage §5.2).
use super::harness;
use serde_json::json;
use zcode_cli_state::node::{entries, resume};

#[tokio::test]
async fn full_access_is_committed_with_nodes_receipt() {
    let mut h = harness::start(Some("full-access"), None).await;
    let session = h.create_in("build", "c1", "write it").await;
    let mut interaction = None;
    for n in 0..250 {
        let (rows, _, _) = h.rows(100 + n, &session).await;
        interaction = rows
            .iter()
            .find_map(|r| r["approvalInteractionId"].as_str().map(str::to_owned));
        if interaction.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let interaction = interaction.expect("permission prompt");
    let ack = h
        .command(
            500,
            json!({"commandId": "c2", "clientId": "cli", "sessionId": session,
            "type": "resolveInteraction", "issuedAt": 1,
            "payload": {"interactionId": interaction, "answer": {"optionId": "fullAccess"}}}),
        )
        .await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let conn = h.settled(&session, 1).await;
    let receipts = entries::list(&conn, &session, Some("runtime/permission_full_access")).unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(
        receipt.id,
        format!("{session}:permission-full-access:{interaction}")
    );
    let event = &receipt.data["event"];
    assert_eq!(event["type"], "session_mode_changed");
    assert_eq!(
        event["payload"],
        json!({"mode": "yolo", "planEnabled": false, "previousMode": "build",
            "previousPlanEnabled": false, "source": "command",
            "permissionGrant": {"interactionId": interaction, "queueItemIds": []}})
    );
    // 会话创建时固定 Rust 实际使用的 shell（Node 的 bash shell 快照）。
    let shells = entries::list(&conn, &session, Some("runtime/bash_shell_selection")).unwrap();
    assert_eq!(shells.len(), 1);
    assert_eq!(
        shells[0].id,
        format!("{session}:runtime:bash_shell_selection")
    );
    let states = entries::list(&conn, &session, Some("runtime/execution_state")).unwrap();
    assert_eq!(states.last().unwrap().data["mode"], "yolo");
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        resumed.permission_grant.as_deref(),
        Some(interaction.as_str())
    );
    harness::dump(&h, &conn, &session);
}

#[tokio::test]
async fn question_auto_resolution_phases_are_node_entries() {
    let mut h = harness::start(Some("question"), None).await;
    let session = h.create("c1", "ask me").await;
    let conn = h.settled(&session, 1).await;
    let phases =
        entries::list(&conn, &session, Some("runtime/user_input_auto_resolution")).unwrap();
    assert_eq!(phases.len(), 1, "one entry per interaction");
    let phase = &phases[0];
    let interaction = phase.data["interactionId"].as_str().unwrap();
    assert_eq!(
        phase.id,
        format!("user-input-auto-resolution:{interaction}")
    );
    let resolution = &phase.data["autoResolution"];
    // 最后一次落库的阶段是倒计时可见；到期应答不再单独记录（与 Node 相同）。
    assert_eq!(resolution["state"], "visibleCountdown");
    assert_eq!(
        phase.time_created,
        resolution["startedAt"].as_i64().unwrap()
    );
    assert!(phase.data["toolCallId"].as_str().is_some());
    assert!(phase.data["turnId"].as_str().unwrap().starts_with("turn_"));
    harness::dump(&h, &conn, &session);
}
