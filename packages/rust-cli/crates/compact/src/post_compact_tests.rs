//! 压缩附件的顺序（compact.md §3.4，附件与 hook 的结果）：
//! 重读的文件、清单增量、SessionStart（`compact`）的消息。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::compaction::PostCompactContext;
use rust_cli_kernel_api::ids::{AgentId, MessageId};
use rust_cli_kernel_api::message::{AttachmentMessage, CompactTrigger, Message};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{
    ContextRequest, HookEvent, HookEventName, HookOutcome, HookRunner, OrchestrationError,
};
use rust_cli_kernel_api::testing::{FakeTool, ListTools};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::test_support::{
    FakeFileStates, Reply, ScriptedRunner, human, ports_with_files, request, tool_round,
};

fn attachment(id: &str, source: &str) -> AttachmentMessage {
    AttachmentMessage {
        id: MessageId::new(id),
        source: source.to_owned(),
        text: format!("<system-reminder>\n{source}\n</system-reminder>"),
        data: Value::Null,
    }
}

/// 记下收到的请求与保留段，回一条延迟工具清单。
#[derive(Default)]
struct Listings(Mutex<Vec<(ContextRequest, usize)>>);

#[async_trait]
impl PostCompactContext for Listings {
    async fn post_compact(
        &self,
        request: ContextRequest,
        kept: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((request, kept.len()));
        Ok(vec![attachment("msg_deferred", "deferred_tools_delta")])
    }
}

struct SessionStart;

#[async_trait]
impl HookRunner for SessionStart {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        if event.name != HookEventName::SessionStart {
            return HookOutcome::default();
        }
        HookOutcome {
            messages: vec![Message::Attachment(attachment(
                "msg_hook",
                "hook_additional_context",
            ))],
            ..HookOutcome::default()
        }
    }
}

async fn compact(agent: Option<AgentId>) -> (Vec<String>, Arc<Listings>) {
    let runner = ScriptedRunner::new(vec![Reply::Text("<summary>S</summary>".to_owned())]);
    let read = Arc::new(FakeTool::new("Read", true, "1\talpha"));
    let files = FakeFileStates::with(&["/Users/dev/project/a.txt"]);
    let mut ports = ports_with_files(runner, Arc::new(ListTools(vec![read])), files);
    let listings = Arc::new(Listings::default());
    ports.context = listings.clone();
    ports.hooks = Arc::new(SessionStart);
    let mut history = vec![human("u1", "go")];
    history.extend(tool_round(1, "Read", "/Users/dev/project/a.txt"));
    history.push(human("u2", "next"));
    let mut request = request(history, CompactTrigger::Reactive);
    request.agent = agent;
    let result = Engine { ports: &ports }
        .run(&request, true, &CancellationToken::new())
        .await
        .unwrap();
    let sources = result
        .restored
        .iter()
        .map(|message| match message {
            Message::Attachment(attachment) => attachment.source.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    (sources, listings)
}

/// 主线程：文件、清单增量、hook 消息；清单按保留段算（这里保留最后一条人类输入）。
#[tokio::test]
async fn listing_deltas_follow_the_files_and_precede_the_hook_messages() {
    let (sources, listings) = compact(None).await;
    assert_eq!(
        sources,
        ["file", "deferred_tools_delta", "hook_additional_context"]
    );
    let calls = listings.0.lock().unwrap();
    let (request, kept) = &calls[0];
    assert_eq!(request.source, QuerySource::MainThread);
    assert_eq!(request.tool_names, ["Read"]);
    assert_eq!(*kept, 1);
}

/// 子代理里的压缩不生成清单增量（compact.md §8）。
#[tokio::test]
async fn subagent_compaction_attaches_no_listings() {
    let (sources, listings) = compact(Some(AgentId::new("agent_1"))).await;
    assert_eq!(sources, ["file", "hook_additional_context"]);
    assert!(listings.0.lock().unwrap().is_empty());
}
