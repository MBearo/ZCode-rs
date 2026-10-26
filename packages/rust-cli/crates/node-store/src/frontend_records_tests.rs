//! 前端记录（缺口 G2）：原样写成 `session_entry`，与 Node 的 `savePersistentCommandFact` 同一行形状。
use rust_cli_kernel_api::store::{FrontendRecord, SessionStore, StoreError};
use serde_json::{Value, json};

use crate::SessionId;
use crate::tests::{TestStore, started};

fn record(key: &str, status: &str) -> FrontendRecord {
    FrontendRecord {
        kind: "app/command_fact".to_owned(),
        key: key.to_owned(),
        data: json!({"source": "timeline", "ack": {"commandId": "c1", "status": status}}),
    }
}

#[tokio::test]
async fn frontend_records_are_session_entries_with_the_given_type_and_id() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let updated = t.rows("select time_updated from session where id = 'sess_1'");
    t.store
        .put_record(&session, &record("fact:c1", "accepted"))
        .await
        .unwrap();
    // 同 key 覆盖。
    t.store
        .put_record(&session, &record("fact:c1", "failed"))
        .await
        .unwrap();
    assert_eq!(
        t.rows("select id, type, data from session_entry where session_id = 'sess_1' and type = 'app/command_fact'"),
        vec![vec![
            Value::from("fact:c1"),
            Value::from("app/command_fact"),
            Value::from(r#"{"source":"timeline","ack":{"commandId":"c1","status":"failed"}}"#),
        ]]
    );
    // 不推进会话的活动时间。
    assert_eq!(
        t.rows("select time_updated from session where id = 'sess_1'"),
        updated
    );
    let read = t.store.records(&session, "app/command_fact").await.unwrap();
    assert_eq!(read, vec![record("fact:c1", "failed")]);
    assert!(t.store.records(&session, "other").await.unwrap().is_empty());
    // 会话不存在时不写。
    assert!(matches!(
        t.store
            .put_record(&SessionId::new("sess_x"), &record("fact:c2", "accepted"))
            .await,
        Err(StoreError::NotFound { .. })
    ));
}
