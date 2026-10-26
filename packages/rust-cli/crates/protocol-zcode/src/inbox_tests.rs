use serde_json::json;

use super::*;

#[test]
fn acks_serialize_like_the_app_schema() {
    let ack = Ack::accepted(
        "c1",
        3,
        Some(json!({"type": "inputAccepted", "delivery": "startNow", "inputId": "c1"})),
    );
    assert_eq!(
        ack.to_value(),
        json!({"commandId": "c1", "status": "accepted", "revisionAtDecision": 3,
            "result": {"type": "inputAccepted", "delivery": "startNow", "inputId": "c1"},
            "memoryEnabled": false})
    );
    let stale = Ack::refused("c2", Status::Stale, "proto.staleRevision", "changed", 9);
    assert_eq!(
        stale.to_value(),
        json!({"commandId": "c2", "status": "stale", "reasonCode": "proto.staleRevision",
            "message": "changed", "revisionAtDecision": 9, "memoryEnabled": false})
    );
    assert!(!stale.is_fact());
    assert_eq!(duplicate(&ack).status, Status::Duplicate);
}

/// 命令事实与 Node 同格式，读回能找到；只认 timeline 与 child 来源。
#[test]
fn command_facts_round_trip_in_the_node_format() {
    let ack = Ack::accepted("c1", 0, None);
    let record = fact(&ack);
    assert_eq!(record.kind, "v4/command_fact");
    assert_eq!(record.key, "v4_command_fact:timeline:c1");
    assert_eq!(record.data["source"], "timeline");
    assert_eq!(find_fact(std::slice::from_ref(&record), "c1"), Some(ack));
    assert_eq!(find_fact(&[record], "c2"), None);
    let other = FrontendRecord {
        kind: COMMAND_FACT.to_owned(),
        key: "x".to_owned(),
        data: json!({"source": "transcript", "ack": Ack::accepted("c3", 0, None).to_value()}),
    };
    assert_eq!(find_fact(&[other], "c3"), None);
}

#[test]
fn the_idempotency_table_forgets_the_oldest() {
    let mut table = Remembered::default();
    for index in 0..(REMEMBERED + 2) {
        table.put(
            key(Some("s"), &format!("c{index}")),
            Ack::accepted("c", 0, None),
        );
    }
    assert!(table.get(&key(Some("s"), "c0")).is_none());
    assert!(table.get(&key(Some("s"), "c2")).is_some());
    assert!(new_session_id().as_str().starts_with("session_"));
    assert_eq!(new_session_id().as_str().len(), "session_".len() + 32);
}
