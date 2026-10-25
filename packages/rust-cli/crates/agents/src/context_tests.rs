//! 子代理的上下文包装：system 段换成子代理的，两组当步 attachment 原样交给主线程的实现（修复线 F16）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId};
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_kernel_api::model::{QuerySource, SystemBlock};
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use serde_json::Value;

use super::SubagentContext;

fn attachment(source: &str) -> AttachmentMessage {
    AttachmentMessage {
        id: MessageId::new(source),
        source: source.to_owned(),
        text: String::new(),
        data: Value::Null,
    }
}

/// 运行开始给 `environment`，工具轮结束给 `total_tokens_reminder`。
struct Inner;

#[async_trait]
impl ContextBuilder for Inner {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        Ok(BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: vec![attachment("date")],
        })
    }

    async fn turn_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        Ok(vec![attachment("total_tokens_reminder")])
    }

    async fn start_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        Ok(vec![attachment("environment")])
    }
}

fn request() -> ContextRequest {
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new("sess_subagent_agent_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/w"),
        },
        agent: None,
        source: QuerySource::Subagent("general-purpose".to_owned()),
        mode: PermissionMode::Default,
        tool_names: Vec::new(),
        selection: selection(),
        plan_flags: Default::default(),
        mcp: None,
    }
}

/// 修 bug（F16）：包装没有转发 `start_attachments` 时落到缺省实现（`turn_attachments`），子代理
/// 第一个请求变成工具轮那一组的顺序（环境段在延迟工具之前）。
#[tokio::test]
async fn both_groups_and_the_announcements_come_from_the_inner_builder() {
    let context = SubagentContext {
        inner: Arc::new(Inner),
        system: vec![SystemBlock {
            text: "Agent prompt.".to_owned(),
            cache_control: None,
        }],
    };
    let start = context.start_attachments(request(), &[]).await.unwrap();
    assert_eq!(start[0].source, "environment");
    let turn = context.turn_attachments(request(), &[]).await.unwrap();
    assert_eq!(turn[0].source, "total_tokens_reminder");
    let built = context.build(request(), &[]).await.unwrap();
    assert_eq!(built.system[0].text, "Agent prompt.");
    assert_eq!(built.attachments[0].source, "date");
}
