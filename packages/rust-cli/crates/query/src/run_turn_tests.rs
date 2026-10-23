//! 当步的 attachment 的两个时点（docs/specs/rust-cli/query.md §1.1，修复线 F10）：运行开始（输入之后、
//! UserPromptSubmit 的结果之前）与每个工具轮结束时（下一次自动压缩之前、轮数检查之前）。旧代码每一步先压缩、
//! 再在 `build` 里一次给出全部 attachment，不调 `turn_attachments`，也不提交 `prompt_hook_messages`。
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::model::{ModelSelection, SystemBlock};
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, BuiltContext, CompactRequest, CompactResult, CompactTracking, Compactor,
    ContextBuilder, ContextRequest, OrchestrationError, QueryStop,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::run::tests::{parts, request, scope};

fn attachment(id: String, source: &str) -> AttachmentMessage {
    AttachmentMessage {
        id: MessageId::new(id.clone()),
        source: source.to_owned(),
        text: format!("<system-reminder>\n{id}\n</system-reminder>"),
        data: Value::Null,
    }
}

/// 当步的 attachment 是 `turn-N`，每一步的上下文宣布是 `announce-N`。
#[derive(Default)]
struct Phased {
    turns: AtomicUsize,
    builds: AtomicUsize,
}

#[async_trait]
impl ContextBuilder for Phased {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        let n = self.builds.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(BuiltContext {
            system: vec![SystemBlock {
                text: "system".to_owned(),
                cache_control: None,
            }],
            user_context: Vec::new(),
            attachments: vec![attachment(format!("announce-{n}"), "date")],
        })
    }

    async fn turn_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        let n = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(vec![attachment(
            format!("turn-{n}"),
            "total_tokens_reminder",
        )])
    }
}

/// 每一步都要自动压缩，但什么都不做：记下每次收到的历史的最后一条消息。
#[derive(Default)]
struct Watching {
    last: Mutex<Vec<String>>,
}

#[async_trait]
impl Compactor for Watching {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        true
    }

    async fn auto_compact(
        &self,
        request: CompactRequest,
        _tracking: &mut CompactTracking,
        _cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        let last = request
            .messages
            .last()
            .map(|message| message.id().as_str().to_owned())
            .unwrap_or_default();
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(last);
        Ok(AutoCompactOutcome::NotNeeded)
    }

    async fn compact(
        &self,
        _request: CompactRequest,
        _cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "not used".to_owned(),
        })
    }
}

fn ids(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.id().as_str().to_owned())
        .collect()
}

/// 运行开始：输入那一步的当步 attachment，再是 UserPromptSubmit 的结果，再是上下文宣布（UPS
/// 上下文排在 `<total_tokens>` 之后、日期之前）。
#[tokio::test]
async fn the_prompt_hook_messages_follow_the_turn_attachments_and_precede_the_announcements() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let mut parts = parts(model, Vec::new());
    parts.context = Arc::new(Phased::default());
    let mut request = request("hi");
    request.prompt_hook_messages = vec![Message::Attachment(attachment(
        "ups".to_owned(),
        "hook_additional_context",
    ))];
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    let messages = sink.messages();
    assert_eq!(&ids(&messages)[..3], ["turn-1", "ups", "announce-1"]);
}

/// 工具轮结束时产生当步的 attachment，下一步的自动压缩看到它（阈值压缩的摘要请求末尾带着当步的
/// `<total_tokens>`）；上下文宣布在压缩之后。
#[tokio::test]
async fn the_auto_compaction_after_a_tool_round_sees_that_rounds_turn_attachments() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({"file_path": "a"})),
        text_response(&["done"]),
    ]));
    let compactor = Arc::new(Watching::default());
    let mut parts = parts(model, Vec::new());
    parts.context = Arc::new(Phased::default());
    parts.compactor = compactor.clone();
    let request = request("read a");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(
        *compactor.last.lock().unwrap(),
        ["turn-1".to_owned(), "turn-2".to_owned()]
    );
    let order: Vec<String> = ids(&sink.messages())
        .into_iter()
        .filter(|id| id.starts_with("turn-") || id.starts_with("announce-"))
        .collect();
    assert_eq!(order, ["turn-1", "announce-1", "turn-2", "announce-2"]);
}

/// 到了轮数上限：当步的 attachment 照常产生并落库，再提交 `max_turns_reached`。
#[tokio::test]
async fn the_turn_attachments_come_before_max_turns_reached() {
    let model = Arc::new(ScriptedModel::new(vec![tool_use_response(
        "toolu_1",
        "Read",
        &json!({}),
    )]));
    let mut parts = parts(model.clone(), Vec::new());
    parts.context = Arc::new(Phased::default());
    let mut limited = request("go");
    limited.max_turns = Some(1);
    let (scope, sink) = scope(parts, &limited, Abort::new());
    let stop = crate::run::run(scope, limited).await.unwrap();
    assert_eq!(stop, QueryStop::MaxTurns);
    assert_eq!(model.requests().len(), 1);
    let messages = sink.messages();
    let tail: Vec<&str> = messages
        .iter()
        .rev()
        .take(2)
        .map(|message| match message {
            Message::Attachment(attachment) => attachment.source.as_str(),
            _ => "other",
        })
        .collect();
    assert_eq!(tail, ["max_turns_reached", "total_tokens_reminder"]);
}

/// 运行开始的一组是 `start-N`，工具轮结束的一组是 `turn-N`（子代理两处不同，修复线 F16）。
#[derive(Default)]
struct Split {
    starts: AtomicUsize,
    turns: AtomicUsize,
}

#[async_trait]
impl ContextBuilder for Split {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        Ok(BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: Vec::new(),
        })
    }

    async fn turn_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        let n = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(vec![attachment(
            format!("turn-{n}"),
            "total_tokens_reminder",
        )])
    }

    async fn start_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        let n = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(vec![attachment(format!("start-{n}"), "environment")])
    }
}

/// 运行开始调 `start_attachments`，工具轮结束调 `turn_attachments`（子代理：进入子循环前产生
/// 开头的一组，每个工具轮结束时产生当步的一组；agents.md §4.3）。旧代码运行开始也调
/// `turn_attachments`。
#[tokio::test]
async fn a_run_starts_with_the_start_attachments_and_rounds_end_with_the_turn_attachments() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({"file_path": "a"})),
        text_response(&["done"]),
    ]));
    let mut parts = parts(model, Vec::new());
    parts.context = Arc::new(Split::default());
    let request = request("read a");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    let order: Vec<String> = ids(&sink.messages())
        .into_iter()
        .filter(|id| id.starts_with("turn-") || id.starts_with("start-"))
        .collect();
    assert_eq!(order, ["start-1", "turn-1"]);
}

/// 发请求之前已中止（修复线 F37，query.md §1.2；会话在输入还没提交时被中断，照常提交输入后带着已中止的信号开主循环）：
/// 当步的 attachment、UserPromptSubmit 的结果、上下文宣布照常落库，不做自动压缩，再写中断标记，不发请求。旧代码在
/// 第一步开头看到中止就返回，宣布与中断标记都没有。
#[tokio::test]
async fn a_run_aborted_before_its_request_announces_and_marks_the_interruption() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["unused"])]));
    let compactor = Arc::new(Watching::default());
    let mut parts = parts(model.clone(), Vec::new());
    parts.context = Arc::new(Phased::default());
    parts.compactor = compactor.clone();
    let mut request = request("hi");
    request.prompt_hook_messages = vec![Message::Attachment(attachment(
        "ups".to_owned(),
        "hook_additional_context",
    ))];
    let abort = Abort::new();
    abort.abort(AbortReason::User);
    let (scope, sink) = scope(parts, &request, abort);
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Aborted);
    let messages = sink.messages();
    assert_eq!(&ids(&messages)[..3], ["turn-1", "ups", "announce-1"]);
    assert!(
        matches!(messages.get(3), Some(Message::User(user))
            if format!("{:?}", user.content).contains("[Request interrupted by user]")),
        "{messages:?}"
    );
    assert_eq!(messages.len(), 4);
    assert!(model.requests().is_empty(), "no request for an aborted run");
    assert!(
        compactor
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no compaction for an aborted run"
    );
}

/// 运行中提交新输入引起的中止（`AbortReason::Submit`）不写中断标记（同 `respond.rs` 的规则），宣布照常。
#[tokio::test]
async fn a_send_now_abort_before_the_request_leaves_no_marker() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["unused"])]));
    let mut parts = parts(model.clone(), Vec::new());
    parts.context = Arc::new(Phased::default());
    let request = request("hi");
    let abort = Abort::new();
    abort.abort(AbortReason::Submit);
    let (scope, sink) = scope(parts, &request, abort);
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Aborted);
    assert_eq!(ids(&sink.messages()), ["turn-1", "announce-1"]);
    assert!(model.requests().is_empty());
}
