#![allow(
    clippy::disallowed_methods,
    reason = "测试直接检查 artifacts 目录与会话库文件"
)]
//! 模式映射、分支切点与 artifact 的端口行为。
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactStore, Fact, RevertCursor, SessionStore, StoreError,
};
use serde_json::{Value, json};

use super::{TestStore, assistant, batch, meta, started, text, user};
use crate::SessionId;

fn sid() -> SessionId {
    SessionId::new("sess_1")
}

/// 旧的单值事实写入后读回判决用的模式（plan 开时是 plan）。
async fn mode(t: &TestStore, mode: PermissionMode) -> Option<PermissionMode> {
    t.store
        .commit(&sid(), batch(None, vec![Fact::ModeChanged { mode }]))
        .await
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    loaded
        .mode
        .map(|mode| ExecutionState::from_parts(mode, loaded.plan).effective())
}

async fn execution(t: &TestStore, state: ExecutionState) -> (Option<PermissionMode>, bool) {
    t.store
        .commit(&sid(), batch(None, vec![Fact::ExecutionChanged { state }]))
        .await
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    (loaded.mode, loaded.plan)
}

fn named(t: &TestStore) -> Vec<Vec<Value>> {
    t.rows("select data from session_entry where type = 'rust-cli/permission_mode'")
}

/// 修复线 F20：执行状态整体写入（权限模式 × plan），会话行在 plan 开时是 `plan`，`rust-cli/permission_mode` 只记
/// 权限模式；读回不变。
#[tokio::test]
async fn execution_states_write_the_node_records_and_read_back() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let cases = [
        (
            ExecutionState::from_parts(PermissionMode::Guarded, true),
            r#"{"mode":"plan"}"#,
            r#"{"mode":"guarded","planEnabled":true}"#,
            r#"{"mode":"guarded"}"#,
        ),
        (
            ExecutionState::from_parts(PermissionMode::Guarded, false),
            r#"{"mode":"guarded"}"#,
            r#"{"mode":"guarded","planEnabled":false}"#,
            r#"{"mode":"guarded"}"#,
        ),
        (
            ExecutionState::from_parts(PermissionMode::DontAsk, true),
            r#"{"mode":"plan"}"#,
            r#"{"mode":"build","planEnabled":true}"#,
            r#"{"mode":"dontAsk"}"#,
        ),
    ];
    for (state, permission, node, entry) in cases {
        assert_eq!(execution(&t, state).await, (Some(state.mode), state.plan));
        assert_eq!(
            node_state(&t),
            vec![vec![Value::from(permission), Value::from(node)]],
            "{state:?}"
        );
        assert_eq!(named(&t), vec![vec![Value::from(entry)]], "{state:?}");
    }
    // 旧的单值 plan 只打开 plan，不改权限模式与 `rust-cli/permission_mode`。
    assert_eq!(
        mode(&t, PermissionMode::Plan).await,
        Some(PermissionMode::Plan)
    );
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        (loaded.mode, loaded.plan),
        (Some(PermissionMode::DontAsk), true)
    );
}

fn node_state(t: &TestStore) -> Vec<Vec<Value>> {
    t.rows(
        "select (select permission from session where id = 'sess_1'), data from session_entry
         where type = 'runtime/execution_state'",
    )
}

/// spec §5：default → build、acceptEdits → edit、plan → plan（Node 执行状态保留模式、打开 plan）、
/// bypassPermissions → yolo、auto → auto；dontAsk 记为 build，靠 `rust-cli/permission_mode` 读回。
#[tokio::test]
async fn modes_map_to_node_execution_states_and_read_back() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let cases = [
        (
            PermissionMode::AcceptEdits,
            r#"{"mode":"edit"}"#,
            r#"{"mode":"edit","planEnabled":false}"#,
        ),
        (
            PermissionMode::Plan,
            r#"{"mode":"plan"}"#,
            r#"{"mode":"edit","planEnabled":true}"#,
        ),
        (
            PermissionMode::BypassPermissions,
            r#"{"mode":"yolo"}"#,
            r#"{"mode":"yolo","planEnabled":false}"#,
        ),
        (
            PermissionMode::DontAsk,
            r#"{"mode":"build"}"#,
            r#"{"mode":"build","planEnabled":false}"#,
        ),
        (
            PermissionMode::Auto,
            r#"{"mode":"auto"}"#,
            r#"{"mode":"auto","planEnabled":false}"#,
        ),
        (
            PermissionMode::Default,
            r#"{"mode":"build"}"#,
            r#"{"mode":"build","planEnabled":false}"#,
        ),
    ];
    for (value, permission, execution) in cases {
        assert_eq!(mode(&t, value).await, Some(value));
        assert_eq!(
            node_state(&t),
            vec![vec![Value::from(permission), Value::from(execution)]],
            "{value:?}"
        );
    }
}

/// Node 续写时改了执行状态：`rust-cli/permission_mode` 过期，以 Node 为准。
#[tokio::test]
async fn a_mode_changed_by_node_wins_over_the_rust_cli_entry() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    assert_eq!(
        mode(&t, PermissionMode::DontAsk).await,
        Some(PermissionMode::DontAsk)
    );
    t.connection()
        .execute(
            "update session_entry set data = '{\"mode\":\"edit\",\"planEnabled\":false}'
             where type = 'runtime/execution_state'",
            [],
        )
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(loaded.mode, Some(PermissionMode::AcceptEdits));
    // 修复线 F20：Node 写的 guarded 与 plan 开关读回 guarded + plan 开（以前 guarded 读成 bypassPermissions）。
    t.connection()
        .execute(
            "update session_entry set data = '{\"mode\":\"guarded\",\"planEnabled\":true}'
             where type = 'runtime/execution_state'",
            [],
        )
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        (loaded.mode, loaded.plan),
        (Some(PermissionMode::Guarded), true)
    );
    // 只有会话行 `mode: plan` 的旧会话（没有执行状态）：plan 开、build。
    t.connection()
        .execute(
            "delete from session_entry where type in ('runtime/execution_state', 'rust-cli/permission_mode')",
            [],
        )
        .unwrap();
    t.connection()
        .execute(
            "update session set permission = '{\"mode\":\"plan\"}' where id = 'sess_1'",
            [],
        )
        .unwrap();
    let loaded = t.store.load(&sid()).await.unwrap();
    assert_eq!(
        (loaded.mode, loaded.plan),
        (Some(PermissionMode::Default), true)
    );
}

/// `RevertSet` 写 Node `session.revert`；冷读取的活动分支 = 保留前缀 + 切点之后追加的消息。
#[tokio::test]
async fn a_revert_cursor_selects_the_active_branch() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![
        user("msg_1_u1", "one"),
        assistant("msg_2_a1", vec![text("first")], None),
        user("msg_3_u2", "two"),
        assistant("msg_4_a2", vec![text("second")], None),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let cursor = RevertCursor {
        target: MessageId::new("msg_3_u2"),
        kept: vec![MessageId::new("msg_1_u1"), MessageId::new("msg_2_a1")],
        cut_after: MessageId::new("msg_4_a2"),
        generation: 1,
    };
    let facts = vec![
        Fact::RevertSet { cursor },
        user("msg_5_u3", "retry two"),
        assistant("msg_6_a3", vec![text("second again")], None),
    ];
    t.store.commit(&sid(), batch(None, facts)).await.unwrap();
    let revert = t.rows("select revert from session");
    assert_eq!(
        revert[0][0],
        Value::from(
            r#"{"keptMessageIDs":["msg_1_u1","msg_2_a1"],"branchCutAfterMessageID":"msg_4_a2","branchGeneration":1,"messageID":"msg_2_a1","kind":"conversation_rewind","scope":"conversation","targetMessageID":"msg_3_u2"}"#
        )
    );
    let ids: Vec<String> = t
        .store
        .load(&sid())
        .await
        .unwrap()
        .messages
        .iter()
        .map(|m| m.id().to_string())
        .collect();
    assert_eq!(ids, ["msg_1_u1", "msg_2_a1", "msg_5_u3", "msg_6_a3"]);
    // 切点之后的 assistant 挂在新的用户输入下。
    let parent =
        t.rows("select json_extract(data, '$.parentID') from message where id = 'msg_6_a3'");
    assert_eq!(parent[0][0], Value::from("msg_5_u3"));
}

/// `Entry` 写成 `rust-cli/<kind>` 前缀的 session_entry，不推进会话活动时间。
#[tokio::test]
async fn rust_cli_entries_are_prefixed_and_do_not_touch_the_session() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    t.source.at(9000);
    let fact = Fact::Entry {
        kind: "tasks".into(),
        id: "list".into(),
        data: json!({"b": 1, "a": [1.0, 2.5]}),
    };
    t.store
        .commit(&sid(), batch(None, vec![fact]))
        .await
        .unwrap();
    let rows = t.rows(
        "select id, type, time_created, data from session_entry where type = 'rust-cli/tasks'",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::from("sess_1:rust-cli/tasks:list"),
            Value::from("rust-cli/tasks"),
            Value::from(9000),
            Value::from(r#"{"b":1,"a":[1,2.5]}"#),
        ]]
    );
    assert_eq!(
        t.rows("select time_updated from session")[0][0],
        Value::from(1000)
    );
    let empty = Fact::Entry {
        kind: String::new(),
        id: "x".into(),
        data: json!({}),
    };
    let refused = t.store.commit(&sid(), batch(None, vec![empty])).await;
    assert!(matches!(refused, Err(StoreError::Storage { .. })));
}

#[tokio::test]
async fn artifacts_round_trip_in_the_node_artifact_directory() {
    let t = TestStore::new();
    let bytes = vec![0_u8, 255, 10, 13];
    let artifact = t
        .store
        .put(
            &SessionId::new("sess/1"),
            ArtifactPurpose::Media,
            "image/png".into(),
            bytes.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        artifact.id.as_str(),
        "tool-result-00000000-0000-4000-8000-000000000001"
    );
    assert_eq!(artifact.size, 4);
    let file = t
        .dir
        .path()
        .join("cli/artifacts/sess_1/media-tool-result-00000000-0000-4000-8000-000000000001.png");
    assert_eq!(rust_cli_fs_read(&file), bytes);
    assert_eq!(t.store.get(&artifact).await.unwrap(), bytes);
    assert_eq!(
        crate::artifact_uri(&artifact),
        "zcode-artifact://sess%2F1/tool-result-00000000-0000-4000-8000-000000000001"
    );
    let mut truncated = artifact.clone();
    truncated.size = 3;
    assert!(matches!(
        t.store.get(&truncated).await,
        Err(StoreError::Storage { .. })
    ));
    let mut missing = artifact;
    missing.id = rust_cli_kernel_api::ids::ArtifactId::new("tool-result-missing");
    assert!(t.store.get(&missing).await.is_err());
}

fn rust_cli_fs_read(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn the_database_is_created_with_its_directory_on_first_use() {
    let t = TestStore::new();
    assert!(!t.store.paths().database().as_path().exists());
    t.store.create(meta("sess_1", "/w", "")).await.unwrap();
    let ledger = t.rows("select count(*) from schema_migration");
    assert_eq!(ledger[0][0], Value::from(24));
}
