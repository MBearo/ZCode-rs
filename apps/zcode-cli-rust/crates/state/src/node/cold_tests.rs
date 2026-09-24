//! Replays `fixtures/node-cold.json` (Node transcripts stored by Node's
//! repositories, with Node's rebuilt history) and requires the same entries.
use super::super::json::stringify;
use super::super::test_db::first_difference;
use super::*;
use serde_json::{Value, json};
use zcode_cli_domain::node_history::hydrate;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../fixtures/node-cold.json")).unwrap()
}

fn database(scenario: &Value) -> (tempfile::TempDir, Connection) {
    super::super::test_db::database(&scenario["tables"])
}

#[test]
fn rebuilt_history_matches_node_for_every_scenario() {
    for (name, scenario) in fixture().as_object().unwrap() {
        let (_dir, conn) = database(scenario);
        let session = scenario["sessionID"].as_str().unwrap();
        let active = active(&conn, session).unwrap();
        let entries: Vec<Value> = hydrate(&active, &|_| None)
            .entries
            .iter()
            .map(|e| e.to_node())
            .collect();
        assert_eq!(
            stringify(&Value::Array(entries)),
            stringify(&scenario["history"]),
            "scenario {name}"
        );
    }
}

#[test]
fn a_leading_compaction_summary_becomes_the_context_summary() {
    let fixture = fixture();
    let (_dir, conn) = database(&fixture["compacted"]);
    let compacted = history(&conn, "sess_compacted", &|_| None).unwrap();
    assert!(
        compacted
            .summary
            .unwrap()
            .starts_with("This session is being continued")
    );
    let roles: Vec<&str> = compacted
        .messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
    assert_eq!(
        compacted.messages[0],
        json!({"role": "user", "content": "two"})
    );

    let (_dir, conn) = database(&fixture["basic"]);
    let basic = history(&conn, "sess_basic", &|_| None).unwrap();
    assert_eq!(basic.summary, None);
    assert_eq!(basic.interrupted_tools, 1);
    let interrupted = basic
        .messages
        .iter()
        .find(|m| m["tool_call_id"] == "call_c")
        .unwrap();
    assert_eq!(interrupted["_zcode_tool_failed"], true);
}

#[test]
fn synthesized_events_match_node_for_every_scenario() {
    for (name, scenario) in fixture().as_object().unwrap() {
        let (_dir, conn) = database(scenario);
        let session = scenario["sessionID"].as_str().unwrap();
        let m = materialization(&conn, session).unwrap();
        let sources = node_rows::Sources {
            goal_entries: &m.goal_entries,
            ..Default::default()
        };
        let events: Vec<Value> = node_rows::cold_events(&m.messages, sources, m.target.as_ref())
            .iter()
            .map(node_rows::Event::to_node)
            .collect();
        if let Some(difference) =
            first_difference("events", &Value::Array(events), &scenario["events"])
        {
            panic!("scenario {name}: {difference}");
        }
    }
}

#[test]
fn replayed_rows_and_state_match_node_for_every_scenario() {
    for (name, scenario) in fixture().as_object().unwrap() {
        let (_dir, conn) = database(scenario);
        let session = scenario["sessionID"].as_str().unwrap();
        let m = materialization(&conn, session).unwrap();
        let sources = node_rows::Sources {
            goal_entries: &m.goal_entries,
            ..Default::default()
        };
        let events = node_rows::cold_events(&m.messages, sources, m.target.as_ref());
        let cold = node_rows::replay(session, &events);
        let rows = Value::Array(cold.rows);
        if let Some(difference) = first_difference("rows", &rows, &scenario["rows"]) {
            panic!("scenario {name}: {difference}");
        }
        let state = Value::Object(cold.state);
        if let Some(difference) = first_difference("state", &state, &scenario["state"]) {
            panic!("scenario {name}: {difference}");
        }
    }
}
