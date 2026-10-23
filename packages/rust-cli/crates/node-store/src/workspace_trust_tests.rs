//! 工作区信任的记录（docs/specs/rust-cli/workspace-trust.md §2.3、node-store.md §4.9）。
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::workspace_trust::TrustKey;

use crate::tests::TestStore;

fn keys(list: &[&str]) -> Vec<TrustKey> {
    list.iter().map(|key| TrustKey::new(*key)).collect()
}

#[tokio::test]
async fn trust_is_one_local_setting_row_per_directory() {
    let test = TestStore::new();
    let work = TrustKey::new("/Users/dev/work");
    test.store.trust_workspace(&work).await.unwrap();
    let rows = test.rows(
        "select scope, scope_id, namespace, key, value, schema_version, time_created, time_updated from local_setting",
    );
    assert_eq!(
        rows,
        vec![vec![
            "directory".into(),
            "/Users/dev/work".into(),
            "rust-cli/workspace-trust".into(),
            "trust_dialog".into(),
            r#"{"hasTrustDialogAccepted":true}"#.into(),
            serde_json::Value::from(1),
            serde_json::Value::from(1000),
            serde_json::Value::from(1000),
        ]]
    );
    // 再写一次是 upsert：只改 value、schema_version 与 time_updated（Node `writeLocalSetting`）。
    test.source.at(2000);
    test.store.trust_workspace(&work).await.unwrap();
    let rows = test.rows("select time_created, time_updated from local_setting");
    assert_eq!(
        rows,
        vec![vec![
            serde_json::Value::from(1000),
            serde_json::Value::from(2000)
        ]]
    );
}

#[tokio::test]
async fn the_first_trusted_candidate_wins_in_the_given_order() {
    let test = TestStore::new();
    let candidates = keys(&["/w/repo", "/w/repo/a/b", "/w/repo/a", "/w"]);
    assert_eq!(
        test.store.trusted_workspace(&candidates).await.unwrap(),
        None
    );
    assert_eq!(test.store.trusted_workspace(&[]).await.unwrap(), None);
    test.store.trust_workspace(&candidates[3]).await.unwrap();
    test.store.trust_workspace(&candidates[2]).await.unwrap();
    // 候选顺序决定结果，不按写入顺序。
    assert_eq!(
        test.store.trusted_workspace(&candidates).await.unwrap(),
        Some(candidates[2].clone())
    );
    assert_eq!(
        test.store
            .trusted_workspace(&candidates[3..])
            .await
            .unwrap(),
        Some(candidates[3].clone())
    );
}

#[tokio::test]
async fn a_row_without_the_flag_does_not_count() {
    let test = TestStore::new();
    // 第一次访问时建库并补齐迁移，之后才能像 Node 那样直接写行。
    assert_eq!(
        test.store.trusted_workspace(&keys(&["/w"])).await.unwrap(),
        None
    );
    for value in [
        "not json",
        "{}",
        r#"{"hasTrustDialogAccepted":false}"#,
        r#"{"hasTrustDialogAccepted":"yes"}"#,
    ] {
        test.connection()
            .execute(
                "insert into local_setting values ('directory', '/w', 'rust-cli/workspace-trust', 'trust_dialog', ?, 1, 1, 1)
                 on conflict(scope, scope_id, namespace, key) do update set value = excluded.value",
                rusqlite::params![value],
            )
            .unwrap();
        assert_eq!(
            test.store.trusted_workspace(&keys(&["/w"])).await.unwrap(),
            None,
            "{value}"
        );
    }
    // 别的命名空间、作用域或键的行不算（例如 Node 的 permission 行）。
    test.connection()
        .execute(
            "insert into local_setting values ('project', '/x', 'rust-cli/workspace-trust', 'trust_dialog', '{\"hasTrustDialogAccepted\":true}', 1, 1, 1),
                                              ('directory', '/y', 'permission', 'trust_dialog', '{\"hasTrustDialogAccepted\":true}', 1, 1, 1)",
            [],
        )
        .unwrap();
    assert_eq!(
        test.store
            .trusted_workspace(&keys(&["/x", "/y"]))
            .await
            .unwrap(),
        None
    );
}
