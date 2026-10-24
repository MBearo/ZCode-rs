//! 子代理的询问交给谁（agents.md §4.1.1）：前台经父工具调用，后台经父会话的会话级入口，都没有时没人回答；
//! 询问写上子代理的名字，外层转发时不改。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::ids::{AgentId, InteractionId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, PermissionAnswer,
};
use rust_cli_kernel_api::testing::{MemoryStore, SequentialIds};
use serde_json::json;
use tokio::sync::mpsc;

use super::{Asks, ChildSink, Parent};
use crate::live::Live;

/// 记下收到的询问与撤回，回答“允许”。
#[derive(Default)]
struct Recorder {
    asked: Mutex<Vec<InteractionRequest>>,
    cancelled: Mutex<Vec<ToolUseId>>,
}

#[async_trait]
impl Interactions for Recorder {
    async fn ask(
        &self,
        _session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        self.asked.lock().unwrap().push(request);
        Ok(InteractionAnswer {
            interaction: InteractionId::new("i1"),
            answer: serde_json::to_value(PermissionAnswer::Allow {
                remember: Vec::new(),
            })
            .unwrap(),
        })
    }

    async fn cancel_ask(&self, _session: &SessionId, tool_use: &ToolUseId) {
        self.cancelled.lock().unwrap().push(tool_use.clone());
    }
}

fn sink(parent: Option<Parent>, asks: Option<Asks>) -> ChildSink {
    ChildSink::new(
        Arc::new(MemoryStore::default()),
        Arc::new(SequentialIds::default()),
        Arc::new(Live::default()),
        AgentId::new("agent_1"),
        SessionId::new("sess_subagent_agent_1"),
        MessageId::new("prompt_1"),
        parent,
    )
    .asking("general-purpose", asks)
}

fn permission(asked_by: Option<&str>) -> InteractionRequest {
    InteractionRequest::Permission {
        machine: None,
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: json!({"command": "touch a"}),
        message: "ask".to_owned(),
        suggestions: Vec::new(),
        asked_by: asked_by.map(str::to_owned),
    }
}

fn asked_by(request: &InteractionRequest) -> Option<&str> {
    match request {
        InteractionRequest::Permission { asked_by, .. } => asked_by.as_deref(),
        _ => None,
    }
}

#[tokio::test]
async fn a_background_agent_asks_through_the_session_entry_with_its_name() {
    let entry = Arc::new(Recorder::default());
    let sink = sink(None, Some(Asks(entry.clone())));
    let answer = sink
        .ask(&SessionId::new("ignored"), permission(None))
        .await
        .unwrap();
    assert_eq!(answer.answer["decision"], "allow");
    let asked = entry.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked_by(&asked[0]), Some("general-purpose"));
    // 撤回也经同一个入口；子代理里再派生的后台子代理拿到同一个入口。
    sink.cancel_ask(&SessionId::new("ignored"), &ToolUseId::new("toolu_1"))
        .await;
    assert_eq!(
        *entry.cancelled.lock().unwrap(),
        vec![ToolUseId::new("toolu_1")]
    );
    let lasting = sink.lasting(&SessionId::new("ignored")).unwrap();
    lasting
        .ask(&SessionId::new("ignored"), permission(Some("Explore")))
        .await
        .unwrap();
    assert_eq!(entry.asked.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn the_innermost_agent_keeps_its_name_when_forwarded() {
    let parent = Arc::new(Recorder::default());
    let (progress, _updates) = mpsc::channel(4);
    let sink = sink(
        Some(Parent {
            session: SessionId::new("sess_main"),
            interactions: parent.clone(),
            progress,
        }),
        None,
    );
    sink.ask(&SessionId::new("ignored"), permission(Some("Explore")))
        .await
        .unwrap();
    let asked = parent.asked.lock().unwrap().clone();
    assert_eq!(asked_by(&asked[0]), Some("Explore"));
    // 前台子代理没有自己的入口：会话级入口来自父工具调用（这个替身不给）。
    assert!(sink.lasting(&SessionId::new("ignored")).is_none());
}

/// 没有入口的后台子代理（`-p`）不询问：权限按“没有询问的入口”回答，管线据此给拒绝的文字（agents.md §4.1.1）。
#[tokio::test]
async fn without_an_entry_a_background_agent_is_not_asked() {
    let sink = sink(None, None);
    let answer = sink
        .ask(&SessionId::new("ignored"), permission(None))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_value::<PermissionAnswer>(answer.answer).unwrap(),
        PermissionAnswer::NoPrompts
    );
    assert!(sink.lasting(&SessionId::new("ignored")).is_none());
}
