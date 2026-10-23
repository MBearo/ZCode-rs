//! 会话的团队项目（node-store.md §4 的 `ModelSelected`、§5 第 5 条，工作线 F49）：Node 的选择 schema 是 strict，
//! 团队项目另存在 `rust-cli/model_connection`，冷读取时与 `runtime/model_selection` 对得上才用。

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::model::{ModelConnection, ModelSelection};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::repo::entries::{self, Entry};
use crate::tests::{TestStore, batch, meta};

const TEAM: &str = "account:bigmodel-team-coding-plan";

fn team_selection(project: &str) -> ModelSelection {
    ModelSelection {
        provider: TEAM.to_owned(),
        model: "GLM-5.3".to_owned(),
        reasoning: Some("high".to_owned()),
        connection: Some(Box::new(ModelConnection {
            product_id: "prod-1".to_owned(),
            organization_id: "org-t".to_owned(),
            project_id: project.to_owned(),
        })),
    }
}

async fn select(t: &TestStore, id: &str, selection: ModelSelection) {
    let facts = vec![Fact::ModelSelected { selection }];
    t.store
        .commit(&SessionId::new(id), batch(None, facts))
        .await
        .unwrap();
}

async fn loaded(t: &TestStore, id: &str) -> ModelSelection {
    t.store
        .load(&SessionId::new(id))
        .await
        .unwrap()
        .model
        .unwrap()
}

fn entry_rows(t: &TestStore, kind: &str) -> Vec<Vec<Value>> {
    t.rows(&format!(
        "select id, data from session_entry where type = '{kind}' order by id"
    ))
}

#[tokio::test]
async fn the_team_project_is_kept_beside_the_node_selection() {
    let t = TestStore::new();
    t.store.create(meta("ses_team", "/w", "")).await.unwrap();
    select(&t, "ses_team", team_selection("p-t")).await;
    // Node 的选择只有严格字段（多一个字段 Node 整个选择都不认）。
    let node = entry_rows(&t, "runtime/model_selection");
    let data: Value = serde_json::from_str(node[0][1].as_str().unwrap()).unwrap();
    assert_eq!(
        data,
        json!({"modelSelection": {"providerId": TEAM, "modelId": "GLM-5.3", "options": {"reasoningLevel": "high"}}})
    );
    let side = entry_rows(&t, "rust-cli/model_connection");
    assert_eq!(side[0][0], json!("ses_team:rust-cli/model_connection"));
    let data: Value = serde_json::from_str(side[0][1].as_str().unwrap()).unwrap();
    assert_eq!(
        data,
        json!({"providerId": TEAM, "modelId": "GLM-5.3",
               "connection": {"productId": "prod-1", "organizationId": "org-t", "projectId": "p-t"}})
    );
    assert_eq!(loaded(&t, "ses_team").await, team_selection("p-t"));
    // 换团队项目：同一条 entry 改写。
    select(&t, "ses_team", team_selection("p-u")).await;
    assert_eq!(entry_rows(&t, "rust-cli/model_connection").len(), 1);
    assert_eq!(loaded(&t, "ses_team").await, team_selection("p-u"));
}

#[tokio::test]
async fn selections_without_a_team_write_nothing_extra() {
    let t = TestStore::new();
    t.store.create(meta("ses_plain", "/w", "")).await.unwrap();
    let mut plain = team_selection("p-t");
    plain.provider = "account:bigmodel-individual-coding-plan".to_owned();
    plain.connection = None;
    select(&t, "ses_plain", plain.clone()).await;
    assert!(entry_rows(&t, "rust-cli/model_connection").is_empty());
    assert_eq!(loaded(&t, "ses_plain").await, plain);
}

#[tokio::test]
async fn a_model_changed_by_node_drops_the_team_project() {
    let t = TestStore::new();
    t.store.create(meta("ses_node", "/w", "")).await.unwrap();
    select(&t, "ses_node", team_selection("p-t")).await;
    // Node 续写时换了模型（Node `persistRuntimeModelSelection`，不碰 rust-cli 的 entry）。
    let node_write = |model: &str| Entry {
        id: "ses_node:runtime-model-selection".to_owned(),
        session_id: "ses_node".to_owned(),
        kind: "runtime/model_selection".to_owned(),
        time_created: 1,
        time_updated: 2,
        data: json!({"providerId": TEAM, "modelId": model}),
        touch_session: false,
    };
    let conn = t.connection();
    entries::save(&conn, &node_write("GLM-5.3-Flash")).unwrap();
    let selection = loaded(&t, "ses_node").await;
    assert_eq!(
        (selection.model.as_str(), selection.connection),
        ("GLM-5.3-Flash", None)
    );
    // Node 换回同一个模型：记下的团队项目又对得上（Node 与 App 按它们的当前连接取团队，platform.md §9.4）。
    entries::save(&conn, &node_write("GLM-5.3")).unwrap();
    let selection = loaded(&t, "ses_node").await;
    assert_eq!(selection.connection.unwrap().project_id, "p-t");
}

#[tokio::test]
async fn a_broken_record_is_ignored() {
    let t = TestStore::new();
    t.store.create(meta("ses_bad", "/w", "")).await.unwrap();
    select(&t, "ses_bad", team_selection("p-t")).await;
    let conn = t.connection();
    for data in [
        json!({"providerId": TEAM, "modelId": "GLM-5.3", "connection": {"productId": "prod-1", "organizationId": " ", "projectId": "p-t"}}),
        json!({"providerId": TEAM, "modelId": "GLM-5.3", "connection": "p-t"}),
    ] {
        let entry = Entry {
            id: "ses_bad:rust-cli/model_connection".to_owned(),
            session_id: "ses_bad".to_owned(),
            kind: "rust-cli/model_connection".to_owned(),
            time_created: 1,
            time_updated: 2,
            data,
            touch_session: false,
        };
        entries::save(&conn, &entry).unwrap();
        assert_eq!(loaded(&t, "ses_bad").await.connection, None);
    }
}
