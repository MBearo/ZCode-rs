//! fork（`Command::Fork`，docs/specs/rust-cli/session-lifecycle.md §6.1）：新会话行 + 复制活动分支。
//!
//! Node 的 message id 全库唯一，所以复制时换新 ID，并映射引用了消息 ID 的字段（工具结果的
//! assistant、压缩边界的摘要与保留段）；tool_use id 不变。文件不动，checkpoint 不复制。

use std::collections::HashMap;

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::ids::{CommandId, MessageId, OwnerEpoch};
use rust_cli_kernel_api::message::{Message, SystemMessage, UserOrigin};
use rust_cli_kernel_api::session_api::Ack;
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionMeta};
use serde_json::Value;

use crate::actor::{Actor, rejected};

/// Node `titleFromInput` 对空输入的标题。Node 在首条输入时才建会话行，所以原会话标题不会为空；本工程
/// 建会话时就有行，还没有人类输入时标题为空，取同一个缺省值。
const UNTITLED: &str = rust_cli_kernel_api::title::UNTITLED;

/// Node `buildForkedSessionInput` 的标题：`Fork of <原会话标题>`，原样拼接（2026-10-18 用户确认，
/// 取代 `/branch` 的 `<第一条输入> (Branch)`，docs/specs/rust-cli/session-lifecycle.md §6.1）。
pub(crate) fn fork_title(parent: &str) -> String {
    let parent = if parent.is_empty() { UNTITLED } else { parent };
    format!("Fork of {parent}")
}

/// 换新 ID 复制消息。
pub(crate) fn copy(messages: &[Message], ids: &dyn Ids) -> Vec<Message> {
    let map: HashMap<MessageId, MessageId> = messages
        .iter()
        .map(|m| (m.id().clone(), ids.message()))
        .collect();
    let mapped = |id: &MessageId| map.get(id).cloned().unwrap_or_else(|| id.clone());
    messages
        .iter()
        .map(|message| {
            let mut message = message.clone();
            match &mut message {
                Message::User(user) => {
                    user.id = mapped(&user.id);
                    if let UserOrigin::ToolResult { assistant } = &mut user.origin {
                        *assistant = mapped(assistant);
                    }
                }
                Message::Assistant(assistant) => assistant.id = mapped(&assistant.id),
                Message::Attachment(attachment) => attachment.id = mapped(&attachment.id),
                Message::System(SystemMessage::CompactBoundary(boundary)) => {
                    boundary.id = mapped(&boundary.id);
                    boundary.summary = mapped(&boundary.summary);
                    if let Some(preserved) = &mut boundary.preserved {
                        preserved.head = mapped(&preserved.head);
                        preserved.anchor = mapped(&preserved.anchor);
                        preserved.tail = mapped(&preserved.tail);
                    }
                }
                Message::System(SystemMessage::Interrupted { id }) => *id = mapped(id),
            }
            message
        })
        .collect()
}

impl Actor {
    pub(crate) async fn fork(&mut self, command: CommandId, at: Option<MessageId>) -> Ack {
        if self.busy() {
            return rejected("a turn is running; fork after it finishes");
        }
        let end = match &at {
            Some(at) => match self.messages.iter().position(|m| m.id() == at) {
                Some(index) => index + 1,
                None => return rejected(format!("{at} is not in the conversation")),
            },
            None => self.messages.len(),
        };
        // 原会话标题的唯一来源是库里的会话行（Node 在 fork 时 `getSession`）：首条输入的标题由存储在
        // 提交时写入，内存里的 `meta.title` 可能还是空的，所以不用它。
        let parent_title = match self.deps.store.load(&self.meta.id).await {
            Ok(loaded) => loaded.meta.title,
            Err(error) => return rejected(error.to_string()),
        };
        let copied = copy(&self.messages[..end], self.deps.ids.as_ref());
        let id = self.deps.ids.session();
        let meta = SessionMeta {
            id: id.clone(),
            kind: SessionKind::Fork,
            parent: Some(self.meta.id.clone()),
            workspace: self.meta.workspace.clone(),
            title: fork_title(&parent_title),
            trace: self.meta.trace.clone(),
            created_at_ms: self.deps.clock.now_ms(),
        };
        if let Err(error) = self.deps.store.create(meta).await {
            return rejected(error.to_string());
        }
        let mut facts = vec![
            Fact::ModelSelected {
                selection: self.selection.clone(),
            },
            Fact::ExecutionChanged {
                state: self.execution,
            },
        ];
        facts.extend(
            copied
                .into_iter()
                .map(|message| Fact::MessageAppended { message }),
        );
        let batch = FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command: Some(command),
            facts,
            turn: None,
        };
        match self.deps.store.commit(&id, batch).await {
            Ok(_) => Ack::Accepted {
                result: Value::String(id.as_str().to_owned()),
            },
            Err(error) => rejected(error.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
