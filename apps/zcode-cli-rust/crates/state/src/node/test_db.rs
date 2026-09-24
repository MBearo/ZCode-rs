//! Test helpers for the Node fixtures: a migrated database holding raw Node
//! rows, and a JSON difference finder that ignores object member order (V4
//! payloads are parsed, not compared as bytes).
use super::json::stringify;
use super::open;
use rusqlite::Connection;
use serde_json::Value;

/// A migrated database holding `tables` (`{table: [[column values...]]}`).
pub fn database(tables: &Value) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = open::open(
        &dir.path().join("db.sqlite"),
        open::MIGRATION_LOCK_WAIT,
        &mut |_| {},
    )
    .unwrap();
    for (table, rows) in tables.as_object().unwrap() {
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

/// The first difference between two JSON values; arrays are ordered, object
/// members are not.
pub fn first_difference(path: &str, left: &Value, right: &Value) -> Option<String> {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => {
                        if let Some(d) = first_difference(&format!("{path}.{key}"), x, y) {
                            return Some(d);
                        }
                    }
                    (x, y) => return Some(format!("{path}.{key}: {x:?} vs {y:?}")),
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
