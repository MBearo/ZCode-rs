//! `/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5，工作线 F75）：从一条可选的用户输入分出新会话。
//!
//! 可选的输入与 `/rewind` 相同（`rewind_plan::selectable`）。新会话带到这一轮为止：复制到这一轮最后一条 assistant，
//! 由存储一次写完（`SessionStore::fork`）；文件不动。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::Ack;
use rust_cli_kernel_api::store::SessionFork;
use serde_json::json;

use crate::actor::{Actor, rejected};
use crate::rewind_plan::selectable;

/// 复制到哪一条（§6.5.1）：`input` 这一轮（到下一条可选的输入之前）里最后一条 assistant；这一轮没有 assistant
/// 时是 `input` 本身。`input` 不是可选的输入时为 None。
///
/// 续写时补的 `No response requested.`（§6.3.1）不算：它是占位，不是这一轮的回答；还没落库时拿它当复制的终点，
/// 存储找不到这条消息、分叉失败。
pub(crate) fn through(messages: &[Message], input: &MessageId) -> Option<MessageId> {
    let start = messages
        .iter()
        .position(|message| message.id() == input && selectable(message).is_some())?;
    let turn = &messages[start + 1..];
    let end = turn
        .iter()
        .position(|message| selectable(message).is_some())
        .unwrap_or(turn.len());
    let answer = turn[..end].iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) if !assistant.is_resume_sentinel() => {
            Some(assistant.id.clone())
        }
        _ => None,
    });
    Some(answer.unwrap_or_else(|| input.clone()))
}

impl Actor {
    /// `ForkFromInput`：空闲时按内存里的活动历史算出终点，交给存储一次写完新会话（§6.5.2）。
    pub(crate) async fn fork_from_input(&self, input: MessageId) -> Ack {
        if self.busy() {
            return rejected("a turn is running; fork after it finishes");
        }
        let Some(through) = through(&self.messages, &input) else {
            return rejected(format!("{input} is not a message to fork from"));
        };
        let child = self.deps.ids.session();
        let fork = SessionFork {
            parent: self.meta.id.clone(),
            child: child.clone(),
            created_at_ms: self.deps.clock.now_ms(),
            through: through.clone(),
            selection: self.selection.clone(),
            mode: self.execution.mode,
            plan: self.execution.plan,
        };
        let receipt = match self.deps.store.fork(fork).await {
            Ok(receipt) => receipt,
            Err(error) => return rejected(error.to_string()),
        };
        // 回执的文字是 Node 分叉对话的原文；`message` 给界面拼中文提示（工作线 F78）。
        let response = format!(
            "Forked session {child} from message {through}: copied {} messages.",
            receipt.copied
        );
        Ack::Accepted {
            result: json!({
                "session": child.as_str(),
                "response": response,
                "copied": receipt.copied,
                "message": through.as_str(),
            }),
        }
    }
}

#[cfg(test)]
#[path = "fork_input_tests.rs"]
mod tests;
