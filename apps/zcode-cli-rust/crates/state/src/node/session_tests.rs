//! Replays `fixtures/node-sessions.json` (sessions written by Node's
//! SqliteSessionStore) and requires Node's session lists and resume facts.
use super::json::stringify;
use super::test_db::{database, first_difference};
use super::{listing, resume};
use serde_json::{Value, json};
use zcode_cli_domain::session_listing::ListParams;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../fixtures/node-sessions.json")).unwrap()
}

fn assert_same(what: &str, actual: &Value, expected: &Value) {
    if let Some(difference) = first_difference(what, actual, expected) {
        panic!(
            "{difference}\nactual:   {}\nexpected: {}",
            stringify(actual),
            stringify(expected)
        );
    }
}

#[test]
fn stored_summaries_match_node_for_local_and_remote_workspaces() {
    let fixture = fixture();
    let (_dir, conn) = database(&fixture["tables"]);
    for (workspace, expected) in fixture["summaries"].as_object().unwrap() {
        let actual = Value::Array(listing::stored_summaries(&conn, workspace).unwrap());
        assert_same(workspace, &actual, expected);
    }
}

#[test]
fn session_list_matches_node() {
    let fixture = fixture();
    let (_dir, conn) = database(&fixture["tables"]);
    for (name, case) in fixture["lists"].as_object().unwrap() {
        let params = ListParams::parse(&case["params"]).unwrap();
        let actual = Value::Array(listing::list(&conn, &params).unwrap());
        assert_same(name, &actual, &case["result"]["sessions"]);
    }
}

#[test]
fn resume_facts_match_node() {
    let fixture = fixture();
    let (_dir, conn) = database(&fixture["tables"]);
    for (id, expected) in fixture["resume"].as_object().unwrap() {
        let Some(r) = resume::resume(&conn, id, &|_| None, None).unwrap() else {
            assert!(expected.is_null(), "{id} should resume");
            continue;
        };
        let todos: Vec<Value> = r
            .todos
            .iter()
            .map(|t| json!({"content": t.content, "status": t.status, "priority": t.priority}))
            .collect();
        let actual = json!({
            "modelSelection": r.model_selection,
            "execution": r.execution,
            "permissionGrant": r.permission_grant,
            "todos": todos,
            "target": r.target.map(|t| t.to_node()),
            "turnNumber": r.turn_number,
            "latestConversationMessageId": r.latest_message,
            "latestAssistantMessageId": r.latest_assistant,
            "latestAssistantTurnId": r.latest_assistant_turn,
            "lastAssistantCompletedAtMs": r.last_assistant_completed,
        });
        assert_same(id, &actual, expected);
    }
}
