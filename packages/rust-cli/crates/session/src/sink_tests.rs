//! 会话级询问入口在父会话关闭之后改走进程级的入口（docs/specs/rust-cli/agents.md §4.1.2，工作线 F72）。
use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::ids::{SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{DetachedEvent, InteractionRequest, Interactions};
use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds};
use serde_json::json;
use tokio::sync::mpsc;

use super::SessionAsks;
use crate::DetachedAsks;

fn question() -> InteractionRequest {
    InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("tu1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "mytool build"}),
        message: "This command requires approval".to_owned(),
        suggestions: Vec::new(),
        asked_by: Some("Explore".to_owned()),
    }
}

/// 一个已经退出的 actor 的入口（请求通道的发送端都丢了）。
fn closed_entry(detached: Option<Arc<DetachedAsks>>) -> SessionAsks {
    let (sender, _receiver) = mpsc::channel(1);
    let requests = sender.downgrade();
    drop(sender);
    SessionAsks {
        requests,
        session: SessionId::new("old"),
        detached,
    }
}

/// 修复（工作线 F72）：以前父会话退出之后询问直接失败（子代理按拒绝处理）；现在转给进程级的入口，回答原样带回。
#[tokio::test]
async fn a_closed_session_asks_through_the_detached_entry() {
    let hub = Arc::new(DetachedAsks::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
    ));
    hub.closed(&SessionId::new("old"), "Old work");
    let mut events = hub.subscribe();
    let entry = Arc::new(closed_entry(Some(hub.clone())));
    let asking = {
        let entry = entry.clone();
        tokio::spawn(async move { entry.ask(&SessionId::new("ignored"), question()).await })
    };
    let Some(DetachedEvent::Asked { ask }) = events.next().await else {
        panic!("asked");
    };
    assert_eq!(
        (ask.session.as_str(), ask.title.as_str()),
        ("old", "Old work")
    );
    hub.resolve(&ask.interaction, json!({"decision": "allow"}))
        .await
        .unwrap();
    let answered = asking.await.unwrap().unwrap();
    assert_eq!(answered.answer, json!({"decision": "allow"}));
    // 撤回也转过去。
    let asking = {
        let entry = entry.clone();
        tokio::spawn(async move { entry.ask(&SessionId::new("ignored"), question()).await })
    };
    assert!(matches!(
        events.next().await,
        Some(DetachedEvent::Resolved { .. })
    ));
    let Some(DetachedEvent::Asked { ask }) = events.next().await else {
        panic!("asked again");
    };
    entry
        .cancel_ask(&SessionId::new("ignored"), &ToolUseId::new("tu1"))
        .await;
    assert!(asking.await.unwrap().is_err());
    assert_eq!(
        events.next().await,
        Some(DetachedEvent::Resolved {
            interaction: ask.interaction
        })
    );
}

/// 没有进程级的入口（测试与嵌入）时照旧按会话已关闭失败。
#[tokio::test]
async fn without_the_entry_a_closed_session_cannot_ask() {
    let entry = closed_entry(None);
    assert!(entry.ask(&SessionId::new("old"), question()).await.is_err());
}
