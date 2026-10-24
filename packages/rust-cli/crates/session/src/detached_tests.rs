//! 父会话关闭之后的询问入口（docs/specs/rust-cli/agents.md §4.1.2，工作线 F72）。
use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::ids::{InteractionId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{DetachedEvent, InteractionRequest};
use rust_cli_kernel_api::permission::PermissionRule;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds};
use serde_json::json;

use super::DetachedAsks;
use crate::test_support::meta;

fn permission(tool_use: &str) -> InteractionRequest {
    InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new(tool_use),
        tool: "Bash".to_owned(),
        input: json!({"command": "mytool build"}),
        message: "This command requires approval".to_owned(),
        suggestions: vec![PermissionRule {
            tool: "Bash".to_owned(),
            content: Some("mytool build *".to_owned()),
        }],
        asked_by: Some("general-purpose".to_owned()),
    }
}

async fn hub() -> (Arc<DetachedAsks>, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::default());
    store.create(meta()).await.unwrap();
    let hub = Arc::new(DetachedAsks::new(
        store.clone(),
        Arc::new(SequentialIds::default()),
    ));
    (hub, store)
}

fn session() -> SessionId {
    meta().id
}

/// 只收因 `/clear`、`/resume` 关闭过的会话的询问，而且要有订阅者。
#[tokio::test]
async fn only_closed_sessions_with_someone_watching_can_ask() {
    let (hub, _) = hub().await;
    assert!(hub.ask(&session(), permission("tu1")).await.is_err());
    hub.closed(&session(), "Fix the build");
    assert!(hub.ask(&session(), permission("tu1")).await.is_err());
    let watching = hub.subscribe();
    drop(watching);
    assert!(hub.ask(&session(), permission("tu1")).await.is_err());
}

/// 询问带会话与标题；回答后询问方拿到回答，“总是允许”的规则落到那个会话的项目规则集，订阅者收到 `Resolved`。
#[tokio::test]
async fn answers_reach_the_asker_and_remember_rules() {
    let (hub, store) = hub().await;
    hub.closed(&session(), "Fix the build");
    let mut events = hub.subscribe();
    let asking = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.ask(&session(), permission("tu1")).await })
    };
    let Some(DetachedEvent::Asked { ask }) = events.next().await else {
        panic!("asked");
    };
    assert_eq!(
        (ask.session.clone(), ask.title.as_str()),
        (session(), "Fix the build")
    );
    assert_eq!(ask.request, permission("tu1"));
    // 后来的订阅者先拿到还没回答的。
    let mut late = hub.subscribe();
    assert_eq!(
        late.next().await,
        Some(DetachedEvent::Asked { ask: ask.clone() })
    );
    let answer =
        json!({"decision": "allow", "remember": [{"tool": "Bash", "content": "mytool build *"}]});
    hub.resolve(&ask.interaction, answer.clone()).await.unwrap();
    let answered = asking.await.unwrap().unwrap();
    assert_eq!(
        (answered.interaction, answered.answer),
        (ask.interaction.clone(), answer)
    );
    assert_eq!(
        events.next().await,
        Some(DetachedEvent::Resolved {
            interaction: ask.interaction.clone()
        })
    );
    let rules = store.project_rules(&meta().workspace).await.unwrap();
    assert_eq!(
        rules.allow,
        vec![PermissionRule {
            tool: "Bash".to_owned(),
            content: Some("mytool build *".to_owned()),
        }]
    );
    // 回答过的不能再回答。
    assert!(
        hub.resolve(&ask.interaction, json!({"decision": "deny"}))
            .await
            .is_err()
    );
    assert!(
        hub.resolve(&InteractionId::new("nope"), json!({}))
            .await
            .is_err()
    );
}

/// 工具被取消：撤回询问，订阅者收到 `Resolved`，询问方得到错误。
#[tokio::test]
async fn cancelled_asks_are_withdrawn() {
    let (hub, _) = hub().await;
    hub.closed(&session(), "");
    let mut events = hub.subscribe();
    let asking = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.ask(&session(), permission("tu1")).await })
    };
    let Some(DetachedEvent::Asked { ask }) = events.next().await else {
        panic!("asked");
    };
    assert_eq!(ask.title, "");
    hub.cancel(&SessionId::new("other"), &ToolUseId::new("tu1"));
    hub.cancel(&session(), &ToolUseId::new("tu2"));
    hub.cancel(&session(), &ToolUseId::new("tu1"));
    assert!(asking.await.unwrap().is_err());
    assert_eq!(
        events.next().await,
        Some(DetachedEvent::Resolved {
            interaction: ask.interaction
        })
    );
}
