//! 执行管线排在 tool_result 前后的消息（hook 的附加上下文、Skill 的正文，docs/specs/rust-cli/ext.md §2.4）
//! 随结果一起按顺序提交。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::orchestration::{QueryStop, ToolExecution, ToolExecutor};
use rust_cli_kernel_api::testing::{DirectExecutor, FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{Tool, ToolProgress, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::tests::tool_uses;
use crate::run::tests::{parts, request, scope};

fn note(text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(text),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "test".to_owned(),
        },
    })
}

/// 在结果前后各加一条消息的执行管线。
struct Wrapping;

#[async_trait]
impl ToolExecutor for Wrapping {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        let mut execution = DirectExecutor.execute(tool, input, context, progress).await;
        execution.before.push(note("before"));
        execution.after.push(note("after"));
        execution
    }
}

#[tokio::test]
async fn messages_before_and_after_the_result_are_committed_in_order() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[("t1", "Skill", json!({}))]),
        text_response(&["ok"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(FakeTool::new("Skill", false, "loaded"))];
    let mut parts = parts(model, tools);
    parts.executor = Arc::new(Wrapping);
    let request = request("go");
    let (scope, sink) = scope(parts, &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Completed
    );
    let shapes: Vec<String> = sink
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(match &user.content[0] {
                ContentBlock::ToolResult { .. } => "result".to_owned(),
                ContentBlock::Text { text } => text.clone(),
                other => format!("{other:?}"),
            }),
            _ => None,
        })
        .collect();
    let at = shapes.iter().position(|shape| shape == "result").unwrap();
    assert_eq!(shapes[at - 1..=at + 1], ["before", "result", "after"]);
}

/// hook 的停止说明（`hook_stopped_continuation`）。
fn stopped(text: &str) -> Message {
    Message::Attachment(rust_cli_kernel_api::message::AttachmentMessage {
        id: MessageId::new("stop"),
        source: "hook_stopped_continuation".to_owned(),
        text: text.to_owned(),
        data: Value::Null,
    })
}

/// 第一个工具调用的 PostToolUse hook 回 `continue: false`：结果之后带停止说明与停止请求。带着中止句柄时，执行完再
/// 中止这次运行（用户在工具结束的同时按了中断）。
struct StopsAfterFirst(Option<Abort>);

#[async_trait]
impl ToolExecutor for StopsAfterFirst {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        let first = context.tool_use.as_str() == "t1";
        let mut execution = DirectExecutor.execute(tool, input, context, progress).await;
        if first {
            execution
                .after
                .push(stopped("PostToolUse:Read hook stopped continuation"));
            execution.prevent_continuation = true;
        }
        if let Some(abort) = &self.0 {
            abort.abort(rust_cli_kernel_api::abort::AbortReason::User);
        }
        execution
    }
}

/// 缺陷 D3（hooks.md §6、query.md §3）：PostToolUse 的 `continue: false` 在本批结果都提交后以 `hook_stopped` 结束，
/// 不再请求模型。以前执行器丢掉了停止请求，回合照常继续，第二个请求照发。
#[tokio::test]
async fn a_hook_stop_ends_the_turn_after_the_whole_batch_is_committed() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[("t1", "Read", json!({})), ("t2", "Read", json!({}))]),
        text_response(&["not requested"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(FakeTool::new("Read", true, "read"))];
    let mut parts = parts(model.clone(), tools);
    parts.executor = Arc::new(StopsAfterFirst(None));
    let request = request("go");
    let (scope, sink) = scope(parts, &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::HookStopped
    );
    assert_eq!(model.requests().len(), 1);
    let shapes: Vec<String> = sink
        .messages()
        .iter()
        .map(|message| match message {
            Message::Assistant(_) => "assistant".to_owned(),
            Message::User(user) => match &user.content[0] {
                ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id.to_string(),
                other => format!("{other:?}"),
            },
            Message::Attachment(attachment) => attachment.source.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    // 两个结果都提交了，停止说明紧跟第一个结果；之后什么都不再提交（没有当步的 attachment）。
    assert_eq!(
        shapes,
        ["assistant", "t1", "hook_stopped_continuation", "t2"]
    );
}

/// 停止请求与中止同时出现时先看中止（query.md §3）：被中止的调用换成拒绝结果，它的停止请求不算；写中断标记，以
/// `Aborted` 结束。
#[tokio::test]
async fn an_abort_wins_over_a_hook_stop() {
    let model = Arc::new(ScriptedModel::new(vec![tool_uses(&[(
        "t1",
        "Read",
        json!({}),
    )])]));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(FakeTool::new("Read", true, "read"))];
    let mut parts = parts(model, tools);
    let abort = Abort::new();
    parts.executor = Arc::new(StopsAfterFirst(Some(abort.clone())));
    let request = request("go");
    let (scope, sink) = scope(parts, &request, abort);
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Aborted
    );
    let messages = sink.messages();
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, Message::Attachment(a)
            if a.source == "hook_stopped_continuation")),
        "an aborted call delivers only the rejection: {messages:?}"
    );
    assert!(matches!(
        messages.last(),
        Some(Message::User(UserMessage {
            origin: UserOrigin::Human,
            ..
        }))
    ));
}
