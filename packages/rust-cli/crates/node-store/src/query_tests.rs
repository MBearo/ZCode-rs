//! 会话管理（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）：App 任务列表的口径、排序、首条输入、改名与软删除。
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::store::{
    SessionKind, SessionOrder, SessionQuery, SessionStore, StoreError,
};
use serde_json::json;

use crate::tests::{TestStore, batch, meta, user};

fn query(root: Option<&str>, order: SessionOrder, limit: usize) -> SessionQuery {
    SessionQuery {
        environment: EnvironmentId::new(crate::LOCAL_ENVIRONMENT),
        root: root.map(EnvPath::new),
        order,
        limit,
    }
}

/// 三个目录的会话：创建时间与活动时间的先后相反（按 Created 与 Updated 排出不同的顺序）。
async fn seeded() -> TestStore {
    let test = TestStore::new();
    for (index, (id, root)) in [
        ("sess_a", "/w/app"),
        ("sess_b", "/w/lib"),
        ("sess_c", "/w/app"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut session = meta(id, root, "");
        session.created_at_ms = 1000 + index as u64;
        test.store.create(session).await.unwrap();
        test.source.at(9000 - index as u64 * 1000);
        test.store
            .commit(
                &SessionId::new(id),
                batch(
                    None,
                    vec![user(&format!("msg_{id}"), &format!("first in {id}"))],
                ),
            )
            .await
            .unwrap();
    }
    test
}

fn ids(page: &rust_cli_kernel_api::store::SessionPage) -> Vec<&str> {
    page.sessions.iter().map(|s| s.id.as_str()).collect()
}

#[tokio::test]
async fn all_projects_are_listed_by_activity_or_creation() {
    let test = seeded().await;
    let page = test
        .store
        .query_sessions(query(None, SessionOrder::Updated, 10))
        .await
        .unwrap();
    assert_eq!(ids(&page), ["sess_a", "sess_b", "sess_c"]);
    assert!(!page.more);
    let first = &page.sessions[0];
    assert_eq!(first.root.as_str(), "/w/app");
    assert_eq!(first.updated_at_ms, 9000);
    assert_eq!(first.created_at_ms, 1000);
    assert_eq!(first.first_prompt.as_deref(), Some("first in sess_a"));
    // 首条输入就是 Node 的首条输入标题（`first_input`）。
    assert_eq!(first.title, "first in sess_a");
    let created = test
        .store
        .query_sessions(query(None, SessionOrder::Created, 10))
        .await
        .unwrap();
    assert_eq!(ids(&created), ["sess_c", "sess_b", "sess_a"]);
    let app = test
        .store
        .query_sessions(query(Some("/w/app"), SessionOrder::Updated, 1))
        .await
        .unwrap();
    assert_eq!(ids(&app), ["sess_a"]);
    assert!(app.more, "two sessions in /w/app, one shown");
}

/// App 的口径：子代理子会话、远端会话、软删除的不列；fork 列出（`task_type = fork`）。
#[tokio::test]
async fn the_query_follows_the_app_task_list() {
    let test = seeded().await;
    let mut child = meta("sess_child", "/w/app", "");
    child.kind = SessionKind::Subagent;
    child.parent = Some(SessionId::new("sess_a"));
    test.store.create(child).await.unwrap();
    let mut fork = meta("sess_fork", "/w/app", "Fork of x");
    fork.kind = SessionKind::Fork;
    fork.parent = Some(SessionId::new("sess_a"));
    test.store.create(fork).await.unwrap();
    let conn = test.connection();
    conn.execute(
        "update session set workspace_id = 'remote:ssh:host:/w/app' where id = 'sess_c'",
        [],
    )
    .unwrap();
    conn.execute(
        "update session set time_deleted = 5 where id = 'sess_b'",
        [],
    )
    .unwrap();
    let page = test
        .store
        .query_sessions(query(None, SessionOrder::Created, 0))
        .await
        .unwrap();
    assert_eq!(ids(&page), ["sess_fork", "sess_a"]);
    // 还没有输入的会话没有首条输入。
    assert_eq!(page.sessions[0].first_prompt, None);
}

#[tokio::test]
async fn renaming_writes_a_custom_title_without_touching_activity() {
    let test = seeded().await;
    let session = SessionId::new("sess_b");
    test.source.at(20_000);
    test.store.rename(&session, "Release notes").await.unwrap();
    assert_eq!(
        test.rows("select title, title_source, time_updated, time_title_updated from session where id = 'sess_b'"),
        [[json!("Release notes"), json!("custom"), json!(8000), json!(20_000)]]
    );
    assert_eq!(
        test.store.load(&session).await.unwrap().meta.title,
        "Release notes"
    );
    let missing = test.store.rename(&SessionId::new("sess_x"), "x").await;
    assert!(
        matches!(missing, Err(StoreError::NotFound { .. })),
        "{missing:?}"
    );
}

#[tokio::test]
async fn deleting_is_a_soft_delete_like_node() {
    let test = seeded().await;
    let session = SessionId::new("sess_a");
    test.source.at(30_000);
    test.store.delete(&session).await.unwrap();
    // 再删一次：时刻不变（Node `coalesce(time_deleted, ?)`）。
    test.source.at(40_000);
    test.store.delete(&session).await.unwrap();
    assert_eq!(
        test.rows("select time_deleted, time_updated from session where id = 'sess_a'"),
        [[json!(30_000), json!(9000)]]
    );
    // 消息保留。
    assert_eq!(
        test.rows("select count(*) from message where session_id = 'sess_a'"),
        [[json!(1)]]
    );
    let page = test
        .store
        .query_sessions(query(None, SessionOrder::Updated, 0))
        .await
        .unwrap();
    assert_eq!(ids(&page), ["sess_b", "sess_c"]);
    assert!(matches!(
        test.store.load(&session).await,
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        test.store.rename(&session, "x").await,
        Err(StoreError::NotFound { .. })
    ));
    let missing = test.store.delete(&SessionId::new("sess_x")).await;
    assert!(
        matches!(missing, Err(StoreError::NotFound { .. })),
        "{missing:?}"
    );
}

/// 首条输入：合成的提醒与工具结果不算，取第一条真实用户输入的第一个文字 part，最多 500 个字符。
#[tokio::test]
async fn the_first_prompt_is_the_first_real_user_text() {
    let test = TestStore::new();
    test.store.create(meta("sess_p", "/w", "")).await.unwrap();
    let long = "é".repeat(600);
    test.store
        .commit(
            &SessionId::new("sess_p"),
            batch(None, vec![user("msg_1", &long), user("msg_2", "second")]),
        )
        .await
        .unwrap();
    let page = test
        .store
        .query_sessions(query(Some("/w"), SessionOrder::Updated, 5))
        .await
        .unwrap();
    let prompt = page.sessions[0].first_prompt.clone().unwrap();
    assert_eq!(prompt.chars().count(), 500);
    assert!(prompt.chars().all(|c| c == 'é'));
}
