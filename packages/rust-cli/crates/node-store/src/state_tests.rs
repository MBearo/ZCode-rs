//! `state::current_user`：语义与查询计划（docs/specs/rust-cli/performance.md §6）。
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{CURRENT_USER_QUERIES, current_user};
use crate::repo::{messages, open, sessions};

const SESSION: &str = "ses_state";

fn db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) =
        open::open(&dir.path().join("db.sqlite"), open::MIGRATION_LOCK_WAIT, 1).unwrap();
    let create = sessions::Create {
        id: SESSION.to_owned(),
        project_id: "prj".to_owned(),
        workspace_id: None,
        parent_id: None,
        trace_id: None,
        task_type: None,
        slug: "slug".to_owned(),
        directory: "/w".to_owned(),
        path: None,
        title: "t".to_owned(),
        title_source: None,
        version: "1".to_owned(),
        permission: None,
        time_created: None,
    };
    sessions::create(&conn, &create, 1).unwrap();
    (dir, conn)
}

/// 存一条消息；`origin` 是 user 的 `semantics.origin`（None 表示没有 semantics）。
fn save(conn: &Connection, id: &str, role: &str, origin: Option<&str>, created: i64) {
    let mut info = json!({
        "id": id,
        "sessionID": SESSION,
        "role": role,
        "time": { "created": created },
        "anchor": { "turnId": format!("turn_{id}") },
    });
    if let Some(origin) = origin {
        info["semantics"] = json!({ "origin": origin });
    }
    if role == "assistant" {
        info["time"]["completed"] = Value::from(created);
    }
    messages::save_message(conn, &info, created).unwrap();
}

fn found(conn: &Connection) -> Option<(String, Option<String>)> {
    current_user(conn, SESSION).unwrap()
}

#[test]
fn the_last_real_user_message_is_the_current_turn() {
    let (_dir, conn) = db();
    assert_eq!(found(&conn), None);
    save(&conn, "msg_1", "user", Some("real_user"), 10);
    save(&conn, "msg_2", "assistant", None, 11);
    save(&conn, "msg_3", "user", Some("real_user"), 12);
    save(&conn, "msg_4", "assistant", None, 13);
    // 系统注入的 user 消息（reminder、工具结果等）不是真实输入。
    save(&conn, "msg_5", "user", Some("system"), 14);
    save(&conn, "msg_6", "user", None, 15);
    save(&conn, "msg_7", "assistant", None, 16);
    assert_eq!(
        found(&conn),
        Some(("msg_3".to_owned(), Some("turn_msg_3".to_owned())))
    );
}

#[test]
fn rows_with_invalid_json_do_not_match() {
    let (_dir, conn) = db();
    save(&conn, "msg_1", "user", Some("real_user"), 10);
    conn.execute(
        "insert into message (id, session_id, time_created, time_updated, data, sequence)
         values ('msg_bad', ?1, 20, 20, '{not json', 50)",
        [SESSION],
    )
    .unwrap();
    assert_eq!(found(&conn).map(|(id, _)| id), Some("msg_1".to_owned()));
}

#[test]
fn legacy_rows_without_a_sequence_sort_first() {
    // 与原来的排序一致：`sequence is null desc` 把没有 sequence 的旧行排在最前。
    let (_dir, conn) = db();
    save(&conn, "msg_old", "user", Some("real_user"), 10);
    save(&conn, "msg_new", "user", Some("real_user"), 20);
    conn.execute(
        "update message set sequence = null where id = 'msg_old'",
        [],
    )
    .unwrap();
    assert_eq!(found(&conn).map(|(id, _)| id), Some("msg_old".to_owned()));
}

#[test]
fn the_lookup_walks_the_sequence_index_without_sorting_the_session() {
    // 回归（L21）：原来的单条 SQL 按 `sequence is null desc` 排序，计划是“用索引找到会话 + 临时
    // B 树整体排序”，每次把会话的全部消息解析一遍 JSON。现在每条 SQL 都沿索引倒序扫、遇到第一条
    // 匹配就停，不能出现整体排序。
    let (_dir, conn) = db();
    for sql in CURRENT_USER_QUERIES {
        let mut plan = conn.prepare(&format!("explain query plan {sql}")).unwrap();
        let steps: Vec<String> = plan
            .query_map([SESSION], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let text = steps.join("\n");
        assert!(
            text.contains("USING INDEX message_session_sequence_idx"),
            "{text}"
        );
        assert!(!text.contains("USE TEMP B-TREE FOR ORDER BY"), "{text}");
    }
}
