use super::*;
use serde_json::json;

#[tokio::test]
async fn identity_list_uses_indexes_for_ordering_and_explicit_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listing.sqlite");
    let _store = Store::open(path.clone()).await.unwrap();
    let conn = Connection::open(path).unwrap();
    let updated = super::super::storage_listing::UPDATED_AT;
    for (query, index) in [
        (
            format!(
                "SELECT id FROM rust_session WHERE workspace='w' ORDER BY {updated} DESC,id DESC"
            ),
            "rust_session_listing",
        ),
        (
            format!("SELECT id FROM rust_session ORDER BY {updated} DESC,id DESC"),
            "rust_session_global_listing",
        ),
        (
            "SELECT id FROM rust_session WHERE id='s'".into(),
            "rust_session_id",
        ),
    ] {
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains(index), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}

#[tokio::test]
async fn close_draft_storage_is_atomic_and_refuses_promoted_history() {
    use crate::contract::SessionStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("close.sqlite");
    let store = Store::open(path.clone()).await.unwrap();
    let mut draft = Session::new(
        "s".into(),
        "w".into(),
        "p".into(),
        "m".into(),
        "none".into(),
        "e".into(),
        1,
    );
    store.commit("w", Some(&mut draft), None).await.unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_close BEFORE INSERT ON rust_command BEGIN SELECT RAISE(FAIL,'injected close ACK failure'); END;").unwrap();
    assert!(
        store
            .discard_draft("w", "s", ("close".into(), json!({"status":"accepted"})))
            .await
            .is_err()
    );
    assert!(store.load_session("w", "s").await.unwrap().is_some());
    assert!(store.load("w").await.unwrap().1.is_empty());
    conn.execute_batch("DROP TRIGGER reject_close").unwrap();
    draft.append_message(json!({"role":"user","content":"first input already persisted"}));
    store.commit("w", Some(&mut draft), None).await.unwrap();
    // 原子首发与延迟清理冲突时，不能把已有 canonical 消息的记录当成空草稿删除。
    assert!(
        store
            .discard_draft("w", "s", ("close".into(), json!({})))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .load_session("w", "s")
            .await
            .unwrap()
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        store
            .load_session("other-workspace", "s")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn single_session_load_does_not_parse_unrelated_histories() {
    use crate::contract::SessionStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.sqlite");
    let store = Store::open(path.clone()).await.unwrap();
    let mut draft = Session::new(
        "s".into(),
        "w".into(),
        "p".into(),
        "m".into(),
        "none".into(),
        "e".into(),
        1,
    );
    store.commit("w", Some(&mut draft), None).await.unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO rust_session VALUES('w','unrelated','not valid JSON')",
        [],
    )
    .unwrap();
    assert!(store.load_session("w", "s").await.unwrap().is_some());
    store
        .discard_draft("w", "s", ("close".into(), json!({"status":"accepted"})))
        .await
        .unwrap();
    assert!(store.load_session("w", "s").await.unwrap().is_none());
    let ack: String = conn
        .query_row(
            "SELECT ack FROM rust_command WHERE workspace='w' AND key='close'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&ack).unwrap()["status"],
        "accepted"
    );
}

#[tokio::test]
async fn migrate_v1_and_append_without_rewriting_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.sqlite");
    let mut original = Session::new(
        "session".into(),
        "workspace".into(),
        "provider".into(),
        "model".into(),
        "none".into(),
        "epoch".into(),
        1,
    );
    original.rows =
        vec![json!({"rowId":1,"kind":"turnHeader","turnId":"old","state":"completedSuccess"})];
    original.messages = vec![json!({"role":"user","content":"old"})];
    let mut legacy = serde_json::to_value(&original).unwrap();
    legacy["rows"] = json!(original.rows);
    legacy["messages"] = json!(original.messages);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE rust_session(workspace TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(workspace,id));").unwrap();
    conn.execute(
        "INSERT INTO rust_session VALUES('workspace','session',?1)",
        [legacy.to_string()],
    )
    .unwrap();
    let store = Store::open(path).await.unwrap();
    let mut session = store.load("workspace").await.unwrap().0.remove(0);
    assert_eq!(session.messages, original.messages);
    store
        .commit(
            "workspace",
            Some(&mut session),
            Some(("input".into(), json!({"status":"accepted"}))),
        )
        .await
        .unwrap();
    // 历史消息不可变：若新的 commit 意外重写旧行，让数据库主动使测试失败。
    conn.execute_batch("CREATE TRIGGER immutable_old_message BEFORE UPDATE ON rust_message WHEN old.ordinal=0 BEGIN SELECT RAISE(FAIL,'old message rewritten'); END;
            CREATE TRIGGER immutable_old_row BEFORE UPDATE ON rust_row WHEN old.ordinal=0 BEGIN SELECT RAISE(FAIL,'old row rewritten'); END;").unwrap();
    session
        .rows
        .push(json!({"rowId":2,"kind":"turnHeader","turnId":"new"}));
    session
        .messages
        .push(json!({"role":"user","content":"new"}));
    store
        .commit("workspace", Some(&mut session), None)
        .await
        .unwrap();
    let (loaded, acks) = store.load("workspace").await.unwrap();
    assert_eq!(loaded[0].rows, session.rows);
    assert_eq!(loaded[0].messages, session.messages);
    assert_eq!(acks["input"]["status"], "accepted");
    let metadata: String = conn
        .query_row("SELECT body FROM rust_session", [], |r| r.get(0))
        .unwrap();
    let metadata: Value = serde_json::from_str(&metadata).unwrap();
    assert!(metadata.get("rows").is_none() && metadata.get("messages").is_none());
}
#[tokio::test]
async fn queue_disposition_and_new_input_commit_or_roll_back_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let store = Store::open(path.clone()).await.unwrap();
    let mut session = Session::new(
        "s".into(),
        "w".into(),
        "p".into(),
        "m".into(),
        "none".into(),
        "e".into(),
        1,
    );
    session
        .messages
        .push(json!({"role":"user","content":"prior"}));
    store
        .commit(
            "w",
            Some(&mut session),
            Some(("queued".into(), json!({"status":"accepted"}))),
        )
        .await
        .unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_new_ack BEFORE INSERT ON rust_command WHEN new.key='next' BEGIN SELECT RAISE(FAIL,'injected ACK failure'); END;").unwrap();
    session
        .messages
        .push(json!({"role":"user","content":"new"}));
    session
        .pending_acks
        .insert("queued".into(), json!({"status":"failed"}));
    assert!(
        store
            .commit(
                "w",
                Some(&mut session),
                Some(("next".into(), json!({"status":"accepted"})))
            )
            .await
            .is_err()
    );
    let (sessions, acks) = store.load("w").await.unwrap();
    assert_eq!(sessions[0].messages.len(), 1);
    assert_eq!(acks["queued"]["status"], "accepted");
    assert!(!acks.contains_key("next"));
    conn.execute_batch("DROP TRIGGER reject_new_ack").unwrap();
    store
        .commit(
            "w",
            Some(&mut session),
            Some(("next".into(), json!({"status":"accepted"}))),
        )
        .await
        .unwrap();
    let (sessions, acks) = store.load("w").await.unwrap();
    assert_eq!(sessions[0].messages.len(), 2);
    assert_eq!(acks["queued"]["status"], "failed");
    assert_eq!(acks["next"]["status"], "accepted");
    assert!(session.pending_acks.is_empty());
}
