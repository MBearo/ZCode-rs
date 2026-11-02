//! 工具自己的后续消息（docs/specs/rust-cli/ext.md §2.4）：排在 tool_result 之后、PostToolUse 的消息之后。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::hooks::{HookEvent, HookOutcome};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, MessageId, OperationId, RunId, SessionId,
};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::testing::{
    FixedGate, RecordingSink, UnusedEnvironment, controller_services, machine,
};
use rust_cli_kernel_api::tool::PromptContext;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

struct Loader;

fn loaded() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_body"),
        content: vec![ContentBlock::Text {
            text: "skill body".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "skill".to_owned(),
        },
    })
}

#[async_trait]
impl Tool for Loader {
    fn name(&self) -> &str {
        "Skill"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(self
            .call_with_messages(input, context, progress)
            .await?
            .output)
    }
    async fn call_with_messages(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolCallOutput, ToolError> {
        Ok(ToolCallOutput {
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: "Launching skill: s".to_owned(),
                }],
                is_error: false,
                display: Value::Null,
            },
            new_messages: vec![loaded()],
        })
    }
}

/// PostToolUse 给一条附加上下文。
struct PostNote;

fn note() -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("msg_note"),
        source: "hook_additional_context".to_owned(),
        text: "post note".to_owned(),
        data: Value::Null,
    })
}

#[async_trait]
impl HookRunner for PostNote {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        let mut outcome = HookOutcome::default();
        if event.name == HookEventName::PostToolUse {
            outcome.messages.push(note());
        }
        outcome
    }
}

fn context() -> ToolUseContext {
    let session = SessionId::new("sess_1");
    let run = RunId::new("run_1");
    let env = Arc::new(UnusedEnvironment);
    ToolUseContext {
        session: session.clone(),
        run: run.clone(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        machine: machine(env.clone()),
        controller: controller_services(env),
        interactions: Arc::new(RecordingSink::default()),
        session_ops: Arc::new(RecordingSink::default()),
        operation: OperationContext::new(
            EnvironmentId::new("local"),
            Generation::new("g1"),
            OperationId::new("op_1"),
            Origin::Agent { session, run },
        ),
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

#[tokio::test]
async fn follow_up_messages_come_after_post_tool_use_messages() {
    let pipeline = Pipeline::new(Arc::new(FixedGate::allow()), Arc::new(PostNote));
    let (progress, _updates) = mpsc::channel(4);
    let execution = pipeline
        .execute(Arc::new(Loader), json!({}), context(), progress)
        .await;
    assert_eq!(
        execution.output.content,
        [ToolResultContent::Text {
            text: "Launching skill: s".to_owned()
        }]
    );
    assert_eq!(execution.after, [note(), loaded()]);
}
