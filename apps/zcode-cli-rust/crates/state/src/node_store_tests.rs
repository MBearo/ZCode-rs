//! NodeStore over a temporary Node database: settings under Node's project
//! ids, commits that keep failed writes, and the listing and index reads.
use super::NodeStore;
use crate::contract::SessionStore;
use crate::domain::node_journal::Admission;
use crate::domain::session::Session;
use crate::domain::session_listing::ListParams;
use serde_json::json;

async fn store(dir: &tempfile::TempDir) -> NodeStore {
    let root = dir.path();
    NodeStore::open(root.join("db/db.sqlite"), root.join("artifacts"))
        .await
        .unwrap()
}

fn session(workspace: &str) -> Session {
    let mut s = Session::new(
        "sess_1".into(),
        workspace.into(),
        "p".into(),
        "m".into(),
        "high".into(),
        "e".into(),
        1,
    );
    s.workspace_path = Some(workspace.into());
    s
}

#[tokio::test]
async fn project_settings_use_nodes_project_ids() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir).await;
    let ruleset = json!({"version": 1, "allow": [{"toolName": "Write"}]});
    store
        .save_project_setting("/", "permission", "ruleset", &ruleset)
        .await
        .unwrap();
    store
        .save_project_setting("/", "permission", "mode", &json!({"mode": "edit"}))
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("db/db.sqlite")).unwrap();
    // 规则由 core 的 projectIdFromDirectory 写入（空 slug 为 session），模式由 bootstrap 写入（default）。
    let scopes: Vec<(String, String)> = conn
        .prepare("select scope_id, key from local_setting order by key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        scopes,
        [
            ("proj_default".into(), "mode".into()),
            ("proj_session".into(), "ruleset".into())
        ]
    );
    let settings = store.project_settings("/").await.unwrap();
    assert_eq!(settings[&("permission".into(), "ruleset".into())], ruleset);
    assert_eq!(
        settings[&("permission".into(), "mode".into())],
        json!({"mode": "edit"})
    );
    assert!(
        store
            .save_project_setting("/", "other", "key", &json!(1))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_failed_commit_keeps_the_writes_and_a_later_commit_applies_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir).await;
    let mut s = session("/w");
    // 会话行尚未写入时账本外键失败：待写记录必须留在会话上，不能丢。
    let admission = |s: &mut Session| {
        s.node_admit_input(
            5,
            Admission {
                queue_id: "queue_c1",
                kind: "sendText",
                payload: json!({"text": "hi", "intent": {"sourceCommandId": "c1"}}),
                delivery: "startNow",
            },
        )
    };
    admission(&mut s);
    assert!(store.commit("/w", Some(&mut s), None).await.is_err());
    assert_eq!(s.node.pending.len(), 1);
    let pending = s.node.take();
    s.node_ensure_created(5, "hi", "0.0.0");
    s.node.pending.extend(pending);
    store.commit("/w", Some(&mut s), None).await.unwrap();
    assert!(s.node.pending.is_empty());
    let ack = store
        .lookup_ack_live("/w", r#"["sess_1","c1"]"#, false)
        .await
        .unwrap();
    // 未提升的输入在冷查询时按重启丢弃结算（Node discardAdmittedOnLoad）。
    assert_eq!(
        ack.unwrap()["reasonCode"],
        "fault.command.inputDiscardedOnRestart"
    );
}

#[tokio::test]
async fn listing_and_index_read_node_session_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir).await;
    let mut s = session("/w");
    s.node_ensure_created(7, "Hello   there", "0.0.0");
    store.commit("/w", Some(&mut s), None).await.unwrap();
    let index = store.load_index("/w").await.unwrap();
    assert_eq!(index["sess_1"]["title"], "Hello there");
    assert_eq!(index["sess_1"]["titleSource"], "generated");
    assert!(store.load_index("/other").await.unwrap().is_empty());
    let listed = store
        .list_sessions(&ListParams::default(), ("/w", "/w"))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let projected = listed[0].projection(None);
    assert_eq!(
        projected["workspace"],
        json!({"workspacePath": "/w", "workspaceKey": "/w"})
    );
    assert_eq!(projected["titleSource"], "first_input");
    let loaded = store.load_session("/w", "sess_1").await.unwrap().unwrap();
    assert!(loaded.node.created);
    assert_eq!(loaded.title, "Hello there");
    assert!(
        store
            .load_session("/other", "sess_1")
            .await
            .unwrap()
            .is_none()
    );
    store.discard_draft("/w", "sess_2", None).await.unwrap();
    assert!(store.load("/w").await.is_err());
}
