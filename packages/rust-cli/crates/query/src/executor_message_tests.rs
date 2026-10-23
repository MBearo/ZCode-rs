//! 执行管线排在 tool_result 前后的消息（hook 的附加上下文、Skill 的正文，docs/specs/rust-cli/ext.md §2.4）
//! 随结果一起按顺序提交。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::orchestration::{QueryStop, ToolExecution, ToolExecutor};
use rust_cli_kernel_api::testing::{DirectExecutor, FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{Tool, ToolOutput, ToolProgress, ToolUseContext};
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
    ) -> ToolOutput {
        DirectExecutor.execute(tool, input, context, progress).await
    }

    async fn execute_full(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        let mut execution =
            ToolExecution::output(self.execute(tool, input, context, progress).await);
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
