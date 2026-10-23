//! 斜杠命令的展开（docs/specs/rust-cli/ext.md §2.7、session-lifecycle.md §3.1）：人类输入开的回合任务里，
//! 先经 [`UserCommands`] 展开，再跑 UserPromptSubmit。展开只给出文字与种类；这里决定消息 ID——第一条
//! 用户输入用 actor 预先生成的 prompt 消息 ID（checkpoint、标题与命令锚点都认它）。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{OperationContext, Origin};
use rust_cli_kernel_api::ids::{OwnerEpoch, RunId, ToolUseId};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::orchestration::Interactions;
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::user_command::{ExpandedKind, Expansion, UserCommands};

use crate::actor::Actor;

/// 回合要提交的输入：消息（按顺序）与是否调模型。
pub(crate) struct Inputs {
    pub messages: Vec<Message>,
    pub query: bool,
    /// 输入是普通输入（不是斜杠命令）：只有这时展开 `@` 提及（docs/specs/rust-cli/tui-input.md §1.1）。
    pub plain: bool,
}

impl Actor {
    /// 展开时 `!` 命令用的工具上下文：操作来源是用户（`Origin::User`），取消令牌是本回合的中止信号。
    pub(crate) fn command_context(
        &self,
        run: &RunId,
        abort: &Abort,
        interactions: Arc<dyn Interactions>,
    ) -> ToolUseContext {
        let operation = self.deps.ids.operation();
        ToolUseContext {
            session: self.meta.id.clone(),
            run: run.clone(),
            tool_use: ToolUseId::new(operation.as_str()),
            agent: None,
            workspace: self.meta.workspace.clone(),
            mode: self.mode(),
            cancel: abort.token().clone(),
            env: self.deps.env.clone(),
            artifacts: self.deps.artifacts.clone(),
            interactions,
            operation: OperationContext {
                environment: self.meta.workspace.environment.clone(),
                generation: self.deps.generation.clone(),
                owner_epoch: OwnerEpoch::LOCAL,
                operation,
                origin: Origin::User {
                    session: Some(self.meta.id.clone()),
                },
            },
        }
    }
}

fn user(id: rust_cli_kernel_api::ids::MessageId, text: String, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id,
        content: vec![ContentBlock::Text { text }],
        origin,
    })
}

/// 展开 `prompt` 消息的文字：不是命令时原样（一条人类输入，调模型）。
pub(crate) async fn expand(
    commands: &dyn UserCommands,
    ids: &dyn Ids,
    prompt: Message,
    context: ToolUseContext,
) -> Inputs {
    let Message::User(original) = &prompt else {
        return Inputs {
            messages: vec![prompt],
            query: true,
            plain: true,
        };
    };
    let text = original
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    match commands.expand(text, context).await {
        Expansion::NotCommand => Inputs {
            messages: vec![prompt.clone()],
            query: true,
            plain: true,
        },
        Expansion::Messages { messages, query } => {
            let mut prompt_id = Some(original.id.clone());
            let messages = messages
                .into_iter()
                .map(|expanded| match expanded.kind {
                    ExpandedKind::Input => {
                        let id = prompt_id.take().unwrap_or_else(|| ids.message());
                        user(id, expanded.text, UserOrigin::Human)
                    }
                    ExpandedKind::Meta { source } => {
                        user(ids.message(), expanded.text, UserOrigin::Meta { source })
                    }
                    // 例如 `-p` 下未知命令的提醒：随 attachment 规则在发给模型前上浮到输入之前。
                    ExpandedKind::Attachment { source } => Message::Attachment(AttachmentMessage {
                        id: ids.message(),
                        source,
                        text: expanded.text,
                        data: serde_json::Value::Null,
                    }),
                })
                .collect();
            Inputs {
                messages,
                query,
                plain: false,
            }
        }
    }
}

#[cfg(test)]
#[path = "slash_tests.rs"]
mod tests;
