use super::*;
use serde_json::json;

const DEAD_PID: i64 = 999_999_999;

async fn write(path: &Path, text: &str) {
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(path, text).await.unwrap();
}

fn record(authority: &Path, owner_pid: i64, owner_id: &str, had_target: bool) -> String {
    json!({"version":2,"stageName":".t.stage-1","transactionId":"tx-1","ownerId":owner_id,
        "ownerPid":owner_pid,"hadTarget":had_target,"mode":"coordinated",
        "authorityPath":authority.to_string_lossy()})
    .to_string()
}

#[tokio::test]
async fn leftovers_of_a_dead_writer_are_recovered_like_node() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t");
    let (backup, transaction) = sidecars(&target);
    // 没有事务记录：目标缺失时回滚到备份。
    write(&backup.join("f"), "old").await;
    assert_eq!(recover(&target).await.unwrap(), target);
    assert_eq!(
        tokio::fs::read_to_string(target.join("f")).await.unwrap(),
        "old"
    );
    // 目标与备份并存：删除备份。
    write(&backup.join("f"), "older").await;
    recover(&target).await.unwrap();
    assert!(!crate::fsx::exists(&backup).await);

    // 协调事务未提交（权威文件没有该事务）：用备份替换新目标。
    let authority = dir.path().join("installed.json");
    write(&authority, r#"{"plugins":[]}"#).await;
    write(&target.join("f"), "new").await;
    write(&backup.join("f"), "committed").await;
    write(&dir.path().join(".t.stage-1/x"), "stage").await;
    write(&transaction, &record(&authority, DEAD_PID, "other", true)).await;
    recover(&target).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(target.join("f")).await.unwrap(),
        "committed"
    );
    assert!(!crate::fsx::exists(&transaction).await);
    assert!(!crate::fsx::exists(&dir.path().join(".t.stage-1")).await);

    // 已提交：保留新目标、删除备份。
    write(&authority, r#"{"plugins":[{"cacheTransactionId":"tx-1"}]}"#).await;
    write(&target.join("f"), "new").await;
    write(&backup.join("f"), "old").await;
    write(&transaction, &record(&authority, DEAD_PID, "other", true)).await;
    recover(&target).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(target.join("f")).await.unwrap(),
        "new"
    );
    assert!(!crate::fsx::exists(&backup).await);
}

#[tokio::test]
async fn a_live_writer_is_read_through_its_committed_generation() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t");
    let (backup, transaction) = sidecars(&target);
    let authority = dir.path().join("installed.json");
    write(&authority, r#"{"plugins":[]}"#).await;
    write(&target.join("f"), "new").await;
    write(&backup.join("f"), "old").await;
    let me = std::process::id() as i64;
    write(&transaction, &record(&authority, me, &OWNER_ID, true)).await;
    // 活跃 writer 的事务未写入权威状态：读备份，且不做任何恢复。
    assert_eq!(recover(&target).await.unwrap(), backup);
    assert!(crate::fsx::exists(&transaction).await);
    write(&authority, r#"{"x":{"cacheTransactionId":"tx-1"}}"#).await;
    assert_eq!(recover(&target).await.unwrap(), target);
}
