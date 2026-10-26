//! fork 子代理（修复线 L-A 的 A1，docs/specs/rust-cli/agents.md §8.1）：交互会话里 `subagent_type: "fork"` 派生的
//! 子代理继承父会话已提交的对话、主线程的 system prompt 与整份工具注册表，总在后台运行。
//!
//! 继承的前缀不落进子会话（§8.1.3）：子会话只存 directive 与自己的消息，前缀留在 [`crate::live::LiveAgent`] 里，
//! 续跑时拼在冷读取之前。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, StopReason, ToolResultContent,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError,
};
use serde_json::Value;

use rust_cli_kernel_api::clock::Ids;

use crate::definitions::{AgentDefinition, AgentSource};

/// fork 的类型名。
pub(crate) const FORK: &str = "fork";
/// fork 的轮数上限（同对照实现）。
const MAX_TURNS: u32 = 200;
/// 子会话里记父会话与切点的 entry（`rust-cli/` 前缀由存储加，node-store.md §4.8.1）。
pub(crate) const PARENT_ENTRY: &str = "fork_parent";

/// fork 的定义（同对照实现）：全部工具、继承模型与模式、没有自己的 system prompt；不进 agent 清单。
pub(crate) fn definition() -> AgentDefinition {
    AgentDefinition {
        agent_type: FORK.to_owned(),
        when_to_use: String::new(),
        tools: Some(vec!["*".to_owned()]),
        disallowed_tools: Vec::new(),
        prompt: String::new(),
        source: AgentSource::BuiltIn,
        model: Some("inherit".to_owned()),
        effort: None,
        max_turns: Some(MAX_TURNS),
        permission_mode: None,
        background: true,
    }
}

/// 子循环开头继承的前缀：父会话已提交的历史，加只带这个 tool_use 的 assistant 与它的“已启动”结果（同对照实现；
/// 流式回复每块一条消息，fork 看到的只有这一块）。
pub(crate) fn prefix(
    ids: &dyn Ids,
    history: Vec<Message>,
    tool_use: &ToolUseId,
    input: Value,
    model: &str,
) -> Vec<Message> {
    let assistant = ids.message();
    let mut messages = history;
    messages.push(Message::Assistant(AssistantMessage {
        id: assistant.clone(),
        content: vec![ContentBlock::ToolUse {
            id: tool_use.clone(),
            name: crate::agent_tool::NAME.to_owned(),
            input,
        }],
        model: model.to_owned(),
        stop_reason: Some(StopReason::ToolUse),
        usage: None,
    }));
    messages.push(Message::User(UserMessage {
        id: ids.message(),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_use.clone(),
            content: vec![ToolResultContent::Text {
                text: rust_cli_prompts::agents::fork_started().to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult { assistant },
    }));
    messages
}

/// fork 的上下文：system 与宣布按主线程组装，开头不加当步 attachment（前缀里已有父会话宣布过的），工具轮之后按子代理的
/// 规则（agents.md §4.3）。
pub(crate) struct ForkContext {
    pub(crate) inner: Arc<dyn ContextBuilder>,
}

#[async_trait]
impl ContextBuilder for ForkContext {
    async fn build(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        // 按主线程组装：system 与父会话的逐字相同，宣布按继承来的历史算（父会话宣布过的不重复）。
        let main = ContextRequest {
            source: QuerySource::MainThread,
            ..request
        };
        self.inner.build(main, history).await
    }

    async fn turn_attachments(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.inner.turn_attachments(request, history).await
    }

    async fn start_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
