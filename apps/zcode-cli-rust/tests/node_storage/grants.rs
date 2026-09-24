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
