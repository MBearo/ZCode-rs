// 回放方式复制自 apps/zcode-cli-rust/crates/state/src/node/fixture_tests.rs（feat/rs 79b6691c55）。
//! 仓储逐字节比对：`fixtures/node-db.json`（复制自 `apps/zcode-cli-rust/crates/state/fixtures`，由
//! `scripts/zcode-cli-rust-node-db-fixtures.mjs` 用 Node 仓储按固定时钟生成）。只回放本 crate 有的
//! 仓储操作（会话、消息、part、entry、标题、切点）；输入账本、todo、目标等操作写的行与它们推进的
//! 会话活动时间不参与比较。
use rusqlite::Connection;
use serde_json::Value;

use super::{entries, messages, open, sessions};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../fixtures/node-db.json")).unwrap()
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn create(input: &Value) -> sessions::Create {
    sessions::Create {
        id: text(&input["id"]).unwrap(),
        project_id: text(&input["projectID"]).unwrap(),
        workspace_id: text(&input["workspaceID"]),
        parent_id: text(&input["parentID"]),
        trace_id: text(&input["traceID"]),
        task_type: text(&input["taskType"]),
        slug: text(&input["slug"]).unwrap(),
        directory: text(&input["directory"]).unwrap(),
        path: text(&input["path"]),
        title: text(&input["title"]).unwrap(),
        title_source: text(&input["titleSource"]),
        version: text(&input["version"]).unwrap(),
        permission: Some(input["permission"].clone()).filter(|p| !p.is_null()),
        time_created: None,
    }
}

/// 回放一个操作；返回 false 表示本 crate 没有这个仓储操作（跳过）。
fn apply(conn: &Connection, o: &Value, now: i64) -> bool {
    match o["op"].as_str().unwrap() {
        "exec" => conn.execute_batch(o["sql"].as_str().unwrap()).unwrap(),
        "createSession" => sessions::create(conn, &create(&o["input"]), now).unwrap(),
        "updateSession" => {
            let input = &o["input"];
            let update = sessions::Update {
                id: text(&input["id"]).unwrap(),
                title: text(&input["title"]),
                title_source: text(&input["titleSource"]),
                expected_title_sources: input["expectedTitleSources"]
                    .as_array()
                    .map(|a| a.iter().filter_map(text).collect())
                    .unwrap_or_default(),
                ..Default::default()
            };
            sessions::update(conn, &update, now).unwrap();
        }
        "setRevert" => {
            let update = sessions::Update {
                id: text(&o["sessionID"]).unwrap(),
                revert: Some(o["revert"].clone()),
                ..Default::default()
            };
            sessions::update(conn, &update, now).unwrap();
        }
        "saveMessage" if o["copyFrom"].is_null() => {
            messages::save_message(conn, &o["info"], now).unwrap();
        }
        "savePart" => messages::save_part(conn, &o["part"], now).unwrap(),
        // Node `removeMessage`：part 随外键删除。
        "removeMessage" => {
            conn.execute(
                "delete from message where id = ? and session_id = ?",
                [
                    o["messageID"].as_str().unwrap(),
                    o["sessionID"].as_str().unwrap(),
                ],
            )
            .unwrap();
        }
        "saveEntry" => {
            let e = &o["entry"];
            let entry = entries::Entry {
                id: text(&e["id"]).unwrap(),
                session_id: text(&e["sessionID"]).unwrap(),
                kind: text(&e["type"]).unwrap(),
                time_created: e["time"]["created"].as_i64().unwrap(),
                time_updated: e["time"]["updated"].as_i64().unwrap(),
                data: e["data"].clone(),
                touch_session: e["touchSession"] != false,
            };
            entries::save(conn, &entry).unwrap();
        }
        _ => return false,
    }
    true
}

fn rows(conn: &Connection, sql: &str) -> Vec<Value> {
    let mut statement = conn.prepare(sql).unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| {
            let values: Vec<Value> = (0..width)
                .map(|i| match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(v) => v.into(),
                    rusqlite::types::ValueRef::Text(v) => {
                        String::from_utf8_lossy(v).into_owned().into()
                    }
                    other => panic!("unexpected column {other:?}"),
                })
                .collect();
            Ok(Value::from(values))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// 夹具里不由本 crate 仓储产生的行：fork 复制（`copyFrom`）与输入账本提升写的消息。
fn produced(row: &Value, skipped: &[String]) -> bool {
    let id = |i: usize| row[i].as_str().unwrap_or("").to_owned();
    !skipped.contains(&id(0)) && !skipped.contains(&id(1))
}

#[test]
fn repositories_write_node_bytes() {
    let fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) =
        open::open(&dir.path().join("db.sqlite"), open::MIGRATION_LOCK_WAIT, 1).unwrap();
    let mut clock = 0;
    let mut skipped: Vec<String> = vec![];
    for o in fixture["operations"].as_array().unwrap() {
        if let Some(now) = o["now"].as_i64() {
            clock = now;
        }
        if !apply(&conn, o, clock) {
            // 输入账本提升（`message`）与 fork 复制（`info` + `copyFrom`）写的消息。
            let message = o["message"]["id"].as_str().or(o["info"]["id"].as_str());
            skipped.extend(message.map(str::to_owned));
        }
    }
    let tables = &fixture["tables"];
    let keep = |table: &str| -> Vec<Value> {
        tables[table]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| produced(row, &skipped))
            .cloned()
            .collect()
    };
    let message = rows(&conn, "select * from message order by rowid");
    assert_eq!(message, keep("message"));
    let part = rows(&conn, "select * from part order by rowid");
    assert_eq!(part, keep("part"));
    let entry = rows(&conn, "select * from session_entry order by rowid");
    assert_eq!(entry, keep("session_entry"));
    // 会话的活动时间（第 17 列）还被跳过的操作推进过，不参与比较。
    let without_activity = |mut row: Value| {
        row[17] = Value::Null;
        row
    };
    let session: Vec<Value> = rows(&conn, "select * from session order by rowid")
        .into_iter()
        .map(without_activity)
        .collect();
    let expected: Vec<Value> = tables["session"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(without_activity)
        .collect();
    assert_eq!(session, expected);
}

/// Node 仓储的读取：消息解码（去掉 legacy 成员）与 entry 解码（模型选择拆壳）。
#[test]
fn repositories_read_like_node() {
    let fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) =
        open::open(&dir.path().join("db.sqlite"), open::MIGRATION_LOCK_WAIT, 1).unwrap();
    let mut clock = 0;
    for o in fixture["operations"].as_array().unwrap() {
        if let Some(now) = o["now"].as_i64() {
            clock = now;
        }
        apply(&conn, o, clock);
    }
    let read = messages::messages(&conn, "sess_a").unwrap();
    let expected = fixture["read"]["messages"].as_array().unwrap();
    // 最后一条（msg_u2）由输入账本提升写入，本 crate 没有该操作。
    assert_eq!(read.len(), expected.len() - 1);
    for (record, expected) in read.iter().zip(expected) {
        assert_eq!(record.info, expected["info"]);
        assert_eq!(Value::from(record.parts.clone()), expected["parts"]);
    }
    let decoded: Vec<Value> = entries::list(&conn, "sess_a", None)
        .unwrap()
        .into_iter()
        .map(|e| {
            serde_json::json!({"id": e.id, "sessionID": e.session_id, "type": e.kind,
                "time": {"created": e.time_created, "updated": e.time_updated}, "data": e.data})
        })
        .collect();
    assert_eq!(Value::from(decoded), fixture["read"]["entries"]);
}
