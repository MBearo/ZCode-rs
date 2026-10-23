//! 本机项目的持久模式（docs/specs/rust-cli/config.md §2.2）：Node 的值与映射、本工程写出的记录。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, SessionStore};

use crate::tests::{TestStore, batch, meta};

const ROOT: &str = "/Users/dev/repo";
const PROJECT: &str = "proj_users-dev-repo";

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    }
}

/// 像 Node `saveProjectPermissionMode` 那样直接写一行。
fn node_writes(test: &TestStore, value: &str) {
    test.connection()
        .execute(
            "insert into local_setting values ('project', ?, 'permission', 'mode', ?, 1, 1, 1)
             on conflict(scope, scope_id, namespace, key) do update set value = excluded.value",
            rusqlite::params![PROJECT, value],
        )
        .unwrap();
}

async fn read(test: &TestStore) -> Option<PermissionMode> {
    test.store.project_mode(&workspace()).await.unwrap()
}

#[tokio::test]
async fn node_values_map_to_kernel_modes() {
    let test = TestStore::new();
    assert_eq!(read(&test).await, None);
    let cases = [
        ("build", Some(PermissionMode::Default)),
        ("edit", Some(PermissionMode::AcceptEdits)),
        ("plan", Some(PermissionMode::Plan)),
        ("yolo", Some(PermissionMode::BypassPermissions)),
        // 修复线 F20：guarded 是 guarded（以前当 bypassPermissions）。
        ("guarded", Some(PermissionMode::Guarded)),
        ("auto", Some(PermissionMode::Auto)),
        // Node `isCollaborationMode` 不认的值当作没有。
        ("fast", None),
    ];
    for (node, expected) in cases {
        node_writes(&test, &format!(r#"{{"mode":"{node}"}}"#));
        assert_eq!(read(&test).await, expected, "{node}");
    }
    for bad in ["not json", "[1]", "{}", r#"{"mode":1}"#] {
        node_writes(&test, bad);
        assert_eq!(read(&test).await, None, "{bad}");
    }
}

#[tokio::test]
async fn the_kernel_mode_name_is_used_only_when_it_agrees() {
    let test = TestStore::new();
    // 第一次使用时建库（补齐迁移）。
    assert_eq!(read(&test).await, None);
    node_writes(
        &test,
        r#"{"mode":"build","rust-cli/permission_mode":"dontAsk"}"#,
    );
    assert_eq!(read(&test).await, Some(PermissionMode::DontAsk));
    // 名字与 Node 的值不一致（例如别的写入只改了 mode）：按 Node 的值读。
    node_writes(
        &test,
        r#"{"mode":"edit","rust-cli/permission_mode":"dontAsk"}"#,
    );
    assert_eq!(read(&test).await, Some(PermissionMode::AcceptEdits));
    node_writes(
        &test,
        r#"{"mode":"guarded","rust-cli/permission_mode":"bypassPermissions"}"#,
    );
    assert_eq!(read(&test).await, Some(PermissionMode::Guarded));
    node_writes(
        &test,
        r#"{"mode":"guarded","rust-cli/permission_mode":"guarded"}"#,
    );
    assert_eq!(read(&test).await, Some(PermissionMode::Guarded));
}

#[tokio::test]
async fn project_mode_set_upserts_the_node_row() {
    let test = TestStore::new();
    let session = SessionId::new("sess_mode");
    test.store
        .create(meta("sess_mode", ROOT, ""))
        .await
        .unwrap();
    let set = |mode| batch(None, vec![Fact::ProjectModeSet { mode }]);
    test.store
        .commit(&session, set(PermissionMode::AcceptEdits))
        .await
        .unwrap();
    let rows = test.rows(
        "select scope, scope_id, namespace, key, value, schema_version, time_created, time_updated from local_setting",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][..4],
        ["project", PROJECT, "permission", "mode"].map(serde_json::Value::from)
    );
    assert_eq!(
        rows[0][4],
        r#"{"mode":"edit","rust-cli/permission_mode":"acceptEdits"}"#
    );
    assert_eq!(rows[0][5], 1);
    assert_eq!(read(&test).await, Some(PermissionMode::AcceptEdits));
    // dontAsk 记为 build，另带内核的模式名（README §5）；只改 value 与 time_updated。
    test.source.at(2000);
    test.store
        .commit(&session, set(PermissionMode::DontAsk))
        .await
        .unwrap();
    let rows = test.rows("select value, time_created, time_updated from local_setting");
    assert_eq!(
        rows[0],
        [
            r#"{"mode":"build","rust-cli/permission_mode":"dontAsk"}"#.into(),
            serde_json::Value::from(1000),
            serde_json::Value::from(2000)
        ]
    );
    assert_eq!(read(&test).await, Some(PermissionMode::DontAsk));
    // Node 再整份写入时名字随之消失，读 Node 的值。
    node_writes(&test, r#"{"mode":"yolo"}"#);
    assert_eq!(read(&test).await, Some(PermissionMode::BypassPermissions));
}
