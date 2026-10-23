//! Node legacy fork 的克隆规则（session-lifecycle.md §6.5.2 的映射表）。
use rusqlite::Connection;
use serde_json::json;

use super::*;

fn map() -> Mapping {
    Mapping::from([("msg_a".to_owned(), "msg_x".to_owned())])
}

#[test]
fn messages_get_new_ids_mapped_parents_and_the_fork_origin() {
    let info = json!({"role": "assistant", "parentID": "msg_a", "time": {"created": 1},
        "anchor": {"turnId": "turn_1", "orderedMessageIds": ["msg_a", "msg_later"],
            "boundaryMessageId": "msg_a",
            "goalBoundary": {"kind": "snapshot", "target": {"targetID": "t", "sessionID": "sess_1",
                "activeInputId": "in"}, "verificationEntryIds": ["v"]}},
        "metadata": {"keep": 1}, "id": "msg_b", "sessionID": "sess_1"});
    let cloned = message(&info, &map(), "sess_2", "msg_y");
    assert_eq!(
        cloned,
        json!({"role": "assistant", "parentID": "msg_x", "time": {"created": 1},
            "anchor": {"turnId": "turn_1", "orderedMessageIds": ["msg_x", "msg_later"],
                "boundaryMessageId": "msg_x",
                "goalBoundary": {"kind": "snapshot", "target": {"targetID": "t", "sessionID": "sess_2",
                    "activeInputId": null, "activeRunStartedAtMs": null, "activeRunLastSeenAtMs": null},
                    "verificationEntryIds": ["v"]}},
            "metadata": {"keep": 1, "forkOrigin": {"sessionId": "sess_1", "messageId": "msg_b"}},
            "id": "msg_y", "sessionID": "sess_2"})
    );
    // 成员顺序按 JS 展开：原有的原地换值，新成员追加。
    let keys: Vec<&String> = cloned.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "role",
            "parentID",
            "time",
            "anchor",
            "metadata",
            "id",
            "sessionID"
        ]
    );
    let user = json!({"role": "user", "parentID": "msg_a", "id": "msg_c", "sessionID": "sess_1"});
    assert_eq!(
        message(&user, &map(), "sess_2", "msg_z")["parentID"],
        "msg_a"
    );
}

#[test]
fn parts_remap_timeline_compaction_and_tool_attachments() {
    let conn = Connection::open_in_memory().unwrap();
    let uuid = || "u".to_owned();
    let ctx = Commit::new(&conn, "sess_2", (36, &uuid), None, &[], None);
    let timeline = json!({"type": "timeline", "anchorMessageId": "msg_gone", "anchorTurnId": "turn_1",
        "timelineType": "context_compaction", "summaryMessageId": "msg_a",
        "id": "part_1", "sessionID": "sess_1", "messageID": "msg_b"});
    let cloned = part(&ctx, &timeline, &map(), "msg_y");
    assert_eq!(
        cloned,
        json!({"type": "timeline", "timelineType": "context_compaction", "summaryMessageId": "msg_x",
            "id": "part_10_u", "sessionID": "sess_2", "messageID": "msg_y",
            "originAnchorMessageId": "msg_gone", "originAnchorTurnId": "turn_1"})
    );
    let compaction = json!({"type": "compaction", "summaryMessageId": "msg_a",
        "compactBoundary": {"summaryMessageIds": ["msg_a"], "turnId": "turn_1",
            "preservedSegment": {"headMessageId": "msg_a", "anchorMessageId": "msg_b",
                "tailMessageId": "msg_a", "extra": 1}}});
    let cloned = part(&ctx, &compaction, &map(), "msg_y");
    assert_eq!(cloned["summaryMessageId"], "msg_x");
    assert_eq!(
        cloned["compactBoundary"],
        json!({"summaryMessageIds": ["msg_x"], "turnId": "turn_1",
            "preservedSegment": {"headMessageId": "msg_x", "anchorMessageId": "msg_b",
                "tailMessageId": "msg_x"}})
    );
    let tool = json!({"type": "tool", "callID": "toolu_1", "state": {"status": "completed",
        "attachments": [{"id": "part_old", "sessionID": "sess_1", "messageID": "msg_b", "mime": "image/png"}]}});
    let cloned = part(&ctx, &tool, &map(), "msg_y");
    assert_eq!(cloned["callID"], "toolu_1");
    assert_eq!(
        cloned["state"]["attachments"][0],
        json!({"id": "part_10_u", "sessionID": "sess_2", "messageID": "msg_y", "mime": "image/png"})
    );
}

#[test]
fn the_forked_goal_status_follows_node() {
    let passed = json!({"status": "completed", "verification": {"passed": true}});
    let failed = json!({"status": "completed", "verification": {"passed": false}});
    assert_eq!(
        forked_status("active", std::slice::from_ref(&passed)),
        "complete"
    );
    assert_eq!(
        forked_status("complete", std::slice::from_ref(&failed)),
        "active"
    );
    assert_eq!(forked_status("complete", &[]), "active");
    assert_eq!(
        forked_status("paused", &[failed, json!({"status": "running"})]),
        "paused"
    );
    assert_eq!(
        forked_status("active", &[passed, json!({"status": "completed"})]),
        "complete"
    );
}
