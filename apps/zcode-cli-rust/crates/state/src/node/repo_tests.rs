//! Edge cases of the Node repositories beyond the replayed fixture.
use super::*;
use rusqlite::Connection;
use serde_json::json;

fn db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = open::open(
        &dir.path().join("db.sqlite"),
        open::MIGRATION_LOCK_WAIT,
        &mut |_| {},
    )
    .unwrap();
    sessions::create(
        &conn,
        &sessions::Create {
            id: "s".into(),
            project_id: "proj".into(),
            slug: "s".into(),
            directory: "/w".into(),
            title: "t".into(),
            version: "0".into(),
            ..Default::default()
        },
        10,
    )
    .unwrap();
    (dir, conn)
}

fn user(id: &str, created: i64) -> serde_json::Value {
    json!({"id": id, "sessionID": "s", "role": "user", "time": {"created": created}, "agent": "a"})
}

#[test]
fn reads_order_by_sequence_with_nulls_last_and_keep_it_on_resave() {
    let (_dir, conn) = db();
    messages::save_message(&conn, &user("m1", 30), None, 30).unwrap();
    messages::save_message(&conn, &user("m2", 20), None, 20).unwrap();
    // 旧版本二进制可能写入 NULL sequence；触发器补齐之前读取把它排在最后。
    conn.execute_batch(
        "drop trigger message_sequence_autofill;
         insert into message (id, session_id, time_created, time_updated, data, sequence)
         values ('m0', 's', 1, 1, '{\"role\":\"user\",\"time\":{\"created\":1}}', null);",
    )
    .unwrap();
    messages::save_message(&conn, &user("m1", 30), None, 40).unwrap();
    let ids: Vec<String> = messages::messages(&conn, "s")
        .unwrap()
        .into_iter()
        .map(|m| m.info["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["m1", "m2", "m0"]);
    let updated: i64 = conn
        .query_row("select time_updated from session where id = 's'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(updated, 30, "touch keeps the maximum");
}

#[test]
fn a_missing_copy_source_fails_and_parts_follow_their_message() {
    let (_dir, conn) = db();
    let copy = messages::CopyFrom {
        session_id: "s",
        id: "absent",
    };
    let error = messages::save_message(&conn, &user("m1", 1), Some(copy), 1).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Storage copy source missing: message/absent")
    );
    messages::save_message(&conn, &user("m1", 1), None, 1).unwrap();
    messages::save_message(&conn, &user("m2", 2), None, 2).unwrap();
    let part = |message: &str| json!({"id": "p", "sessionID": "s", "messageID": message, "type": "step-start"});
    messages::save_part(&conn, &part("m1"), None, 5).unwrap();
    messages::save_part(
        &conn,
        &json!({"id": "q", "sessionID": "s", "messageID": "m2", "type": "step-start"}),
        None,
        6,
    )
    .unwrap();
    // 改绑到另一条消息时取新 scope 的队尾序号。
    messages::save_part(&conn, &part("m2"), None, 7).unwrap();
    let rows: Vec<(String, String, i64)> = conn
        .prepare("select id, message_id, sequence from part order by id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        [("p".into(), "m2".into(), 1), ("q".into(), "m2".into(), 0)]
    );
    messages::remove_message(&conn, "s", "m2").unwrap();
    let parts: i64 = conn
        .query_row("select count(*) from part", [], |r| r.get(0))
        .unwrap();
    assert_eq!(parts, 0);
}

#[test]
fn entries_require_data_and_settings_fall_back_to_the_legacy_table() {
    let (_dir, conn) = db();
    let entry = entries::Entry {
        id: "e".into(),
        session_id: "s".into(),
        kind: "x".into(),
        time_created: 1,
        time_updated: 1,
        data: serde_json::Value::Null,
        touch_session: true,
    };
    assert!(entries::save(&conn, &entry).is_err());
    assert_eq!(entries::owner(&conn, "e").unwrap(), None);
    conn.execute(
        "insert into permission (project_id, time_created, time_updated, data) values ('proj', 1, 1, '{\"allow\":[\"Bash\"]}')",
        [],
    )
    .unwrap();
    assert_eq!(
        settings::project_permission(&conn, "proj").unwrap(),
        Some(json!({"allow": ["Bash"]}))
    );
    settings::write(
        &conn,
        "project",
        "proj",
        "permission",
        "mode",
        "{\"mode\":\"bogus\"}",
        2,
    )
    .unwrap();
    assert_eq!(
        settings::project_permission_mode(&conn, "proj").unwrap(),
        None
    );
    assert_eq!(todos::read(&conn, "s").unwrap(), []);
    assert_eq!(targets::read(&conn, "s").unwrap(), None);
    assert!(!targets::clear(&conn, "s", 3).unwrap());
}
