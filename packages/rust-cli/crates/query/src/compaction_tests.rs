//! 主循环里的自动与反应式压缩（compact.md §4.1）：用假压缩器，检查提交的消息与下一次请求。
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, SystemMessage,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ApiRole, ModelError, ModelSelection};
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor,
    OrchestrationError, QueryError, QueryStop,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio_util::sync::CancellationToken;

use crate::run::tests::{parts, request, scope};

/// 自动压缩按脚本回答；反应式压缩总是成功。记下收到的触发与初始 token 差。`blocks` 为真时每一步都到了阻塞线，
/// 记下阻塞线收到的压缩失败说明。
struct FakeCompactor {
    auto: Mutex<Vec<AutoCompactOutcome>>,
    calls: Mutex<Vec<CompactTrigger>>,
    gaps: Mutex<Vec<Option<u64>>>,
    blocks: bool,
    failures: Mutex<Vec<Option<String>>>,
}

impl FakeCompactor {
    fn new(auto: Vec<AutoCompactOutcome>) -> Arc<Self> {
        Arc::new(Self {
            auto: Mutex::new(auto.into_iter().rev().collect()),
            calls: Mutex::default(),
            gaps: Mutex::default(),
            blocks: false,
            failures: Mutex::default(),
        })
    }

    fn blocking(auto: Vec<AutoCompactOutcome>) -> Arc<Self> {
        Arc::new(Self {
            auto: Mutex::new(auto.into_iter().rev().collect()),
            calls: Mutex::default(),
            gaps: Mutex::default(),
            blocks: true,
            failures: Mutex::default(),
        })
    }
}

fn result(trigger: CompactTrigger) -> CompactResult {
    CompactResult {
        boundary: CompactBoundary {
            id: MessageId::new("msg_boundary"),
            trigger,
            pre_tokens: 10,
            summary: MessageId::new("msg_summary"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
        summary: UserMessage {
            id: MessageId::new("msg_summary"),
            content: vec![ContentBlock::Text {
                text: "SUMMARY".to_owned(),
            }],
            origin: UserOrigin::CompactSummary,
        },
        restored: vec![Message::Attachment(AttachmentMessage {
            id: MessageId::new("msg_file"),
            source: "file".to_owned(),
            text: "<system-reminder>\nFILE\n</system-reminder>".to_owned(),
            data: serde_json::Value::Null,
        })],
    }
}

#[async_trait]
impl Compactor for FakeCompactor {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        !self
            .auto
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    async fn auto_compact(
        &self,
        _request: CompactRequest,
        _tracking: &mut CompactTracking,
        _cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        let mut auto = self.auto.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(auto.pop().unwrap_or(AutoCompactOutcome::NotNeeded))
    }

    async fn compact(
        &self,
        request: CompactRequest,
        _cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.trigger);
        self.gaps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.initial_token_gap);
        Ok(result(request.trigger))
    }

    fn blocking_limit(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(failure.map(str::to_owned));
        self.blocks.then(|| "Prompt is too long".to_owned())
    }
}

fn texts(request: &rust_cli_kernel_api::model::ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|item| match &item.block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn auto_compaction_commits_together_and_the_request_sees_only_the_active_history() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::new(vec![AutoCompactOutcome::Compacted {
        result: Box::new(result(CompactTrigger::Auto)),
    }]);
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("old question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    let ids: Vec<String> = sink
        .messages()
        .iter()
        .map(|m| m.id().as_str().to_owned())
        .collect();
    assert_eq!(&ids[..3], ["msg_boundary", "msg_summary", "msg_file"]);
    assert!(matches!(
        sink.messages()[0],
        Message::System(SystemMessage::CompactBoundary(_))
    ));
    let sent = model.requests();
    assert_eq!(sent.len(), 1);
    let seen = texts(&sent[0]);
    assert!(seen.iter().any(|text| text == "SUMMARY"), "{seen:?}");
    assert!(
        !seen.iter().any(|text| text.contains("old question")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn prompt_too_long_compacts_once_and_resends() {
    let model = Arc::new(ScriptedModel::new(vec![
        vec![Err(ModelError::PromptTooLong {
            message: "prompt is too long: 300 tokens > 200 maximum".to_owned(),
        })],
        text_response(&["after compaction"]),
    ]));
    let compactor = FakeCompactor::new(Vec::new());
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(
        *compactor.calls.lock().unwrap(),
        vec![CompactTrigger::Reactive]
    );
    // 触发它的错误给出的差交给压缩核心（compact.md §3.1）。
    assert_eq!(*compactor.gaps.lock().unwrap(), vec![Some(100)]);
    let sent = model.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].messages[0].role, ApiRole::User);
    assert!(texts(&sent[1]).iter().any(|text| text == "SUMMARY"));
    assert!(
        sink.messages()
            .iter()
            .any(|m| m.id().as_str() == "msg_boundary")
    );
}

#[tokio::test]
async fn a_second_prompt_too_long_in_the_same_round_ends_with_the_error() {
    let ptl = || {
        vec![Err(ModelError::PromptTooLong {
            message: "prompt is too long".to_owned(),
        })]
    };
    let model = Arc::new(ScriptedModel::new(vec![ptl(), ptl()]));
    let mut parts = parts(model, Vec::new());
    parts.compactor = FakeCompactor::new(Vec::new());
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let error = crate::run::run(scope, request).await.unwrap_err();
    assert!(matches!(
        error,
        QueryError::Model(ModelError::PromptTooLong { .. })
    ));
}

#[tokio::test]
async fn thrashing_ends_the_run_with_the_compactor_message() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = FakeCompactor::new(vec![AutoCompactOutcome::Thrashing {
        message: "Autocompact is thrashing".to_owned(),
    }]);
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(
        stop,
        QueryStop::ApiError {
            message: "Autocompact is thrashing".to_owned()
        }
    );
    assert!(model.requests().is_empty());
}

/// 到了阻塞线（compact.md §2.2）：不发请求，以 API 错误结束；这一步自动压缩失败的说明交给阻塞线。
#[tokio::test]
async fn the_blocking_line_ends_the_run_without_a_request() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let compactor = FakeCompactor::blocking(vec![AutoCompactOutcome::Failed {
        message: "Error during compaction: boom".to_owned(),
    }]);
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(
        stop,
        QueryStop::ApiError {
            message: "Prompt is too long".to_owned()
        }
    );
    assert!(model.requests().is_empty());
    assert_eq!(
        *compactor.failures.lock().unwrap(),
        vec![Some("Error during compaction: boom".to_owned())]
    );
}

/// 刚压缩过的那一步不看阻塞线；没到阻塞线时每一步照常查（没有失败说明）。
#[tokio::test]
async fn a_step_that_just_compacted_is_not_blocked() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::blocking(vec![AutoCompactOutcome::Compacted {
        result: Box::new(result(CompactTrigger::Auto)),
    }]);
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(model.requests().len(), 1);
    assert!(compactor.failures.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_step_checks_the_blocking_line() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::new(Vec::new());
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Completed
    );
    assert_eq!(*compactor.failures.lock().unwrap(), vec![None]);
}
