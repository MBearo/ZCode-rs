//! Replays `fixtures/node-cold.json` (Node transcripts stored by Node's
//! repositories, with Node's rebuilt history) and requires the same entries.
use super::super::json::stringify;
use super::super::open;
use super::*;
use serde_json::{Value, json};
use zcode_cli_domain::node_history::hydrate;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../fixtures/node-cold.json")).unwrap()
}

/// A migrated database holding the scenario's raw rows.
fn database(scenario: &Value) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = open::open(
        &dir.path().join("db.sqlite"),
        open::MIGRATION_LOCK_WAIT,
        &mut |_| {},
    )
    .unwrap();
    for (table, rows) in scenario["tables"].as_object().unwrap() {
        for row in rows.as_array().unwrap() {
            let values: Vec<rusqlite::types::Value> = row
                .as_array()
                .unwrap()
                .iter()
                .map(|v| match v {
                    Value::Null => rusqlite::types::Value::Null,
                    Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
                    Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    other => panic!("unexpected column {other}"),
                })
                .collect();
            let slots = vec!["?"; values.len()].join(",");
            conn.execute(
                &format!("insert into {table} values ({slots})"),
                rusqlite::params_from_iter(values),
            )
            .unwrap();
        }
    }
    (dir, conn)
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

/// First difference between two JSON values (object member order ignored:
/// V4 wire payloads are parsed, not compared as bytes).
fn first_difference(path: &str, left: &Value, right: &Value) -> Option<String> {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                let (x, y) = (a.get(key), b.get(key));
                match (x, y) {
                    (Some(x), Some(y)) => {
                        if let Some(d) = first_difference(&format!("{path}.{key}"), x, y) {
                            return Some(d);
                        }
                    }
                    _ => return Some(format!("{path}.{key}: {x:?} vs {y:?}")),
                }
            }
            None
        }
        (Value::Array(a), Value::Array(b)) => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                if let Some(d) = first_difference(&format!("{path}[{index}]"), x, y) {
                    return Some(d);
                }
            }
            (a.len() != b.len()).then(|| format!("{path}: length {} vs {}", a.len(), b.len()))
        }
        _ => (stringify(left) != stringify(right)).then(|| format!("{path}: {left} vs {right}")),
    }
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
