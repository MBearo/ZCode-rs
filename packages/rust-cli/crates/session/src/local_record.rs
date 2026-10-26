//! 本地命令写进对话的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：固定说明、命令输入、输出与 meta
//! 消息都是 meta user 消息（不是人类输入：不开轮次、不改标题），模型下一个请求看得到。
//!
//! 时机（§11.3）：空闲时马上提交一批；回合或压缩在跑时留在这里，这次工作收尾、开下一个回合之前提交（`start_next`）。

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{Ack, KernelEvent};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::user_command::{
    LOCAL_COMMAND_CAVEAT_SOURCE, LOCAL_COMMAND_INPUT_SOURCE, LOCAL_COMMAND_META_SOURCE,
    LOCAL_COMMAND_OUTPUT_SOURCE, LocalCommandRecord,
};

use crate::actor::{Actor, accepted};

/// 这条 user 消息是本地命令的记录（固定说明、命令输入、输出或记录带的 meta）。
pub(crate) fn is_record(user: &UserMessage) -> bool {
    matches!(&user.origin, UserOrigin::Meta { source } if [
        LOCAL_COMMAND_CAVEAT_SOURCE,
        LOCAL_COMMAND_INPUT_SOURCE,
        LOCAL_COMMAND_OUTPUT_SOURCE,
        LOCAL_COMMAND_META_SOURCE,
    ]
    .contains(&source.as_str()))
}

/// 一条记录的消息，按提交顺序。
pub(crate) fn record_messages(ids: &dyn Ids, record: &LocalCommandRecord) -> Vec<Message> {
    record
        .texts()
        .into_iter()
        .map(|(source, text)| {
            Message::User(UserMessage {
                id: ids.message(),
                content: vec![ContentBlock::Text { text }],
                origin: UserOrigin::Meta {
                    source: source.to_owned(),
                },
            })
        })
        .collect()
}

impl Actor {
    /// `Command::RecordLocalCommand`：空闲时提交，忙时留到收尾。提交失败只记日志（记录是给模型的上下文，丢了不影响
    /// 命令本身已经生效的结果）。
    pub(crate) async fn record_local_command(
        &mut self,
        command: CommandId,
        record: LocalCommandRecord,
    ) -> Ack {
        if self.busy() {
            self.held_records.push(record);
            return accepted();
        }
        let messages = record_messages(self.deps.ids.as_ref(), &record);
        if let Err(reason) = self.commit_record_messages(Some(command), messages).await {
            tracing::warn!(%reason, command = %record.name, "the local command record could not be stored");
        }
        accepted()
    }

    /// 回合或压缩收尾后：把这期间到达的记录按到达顺序一批提交。
    pub(crate) async fn flush_held_records(&mut self) {
        if self.busy() || self.held_records.is_empty() {
            return;
        }
        let records = std::mem::take(&mut self.held_records);
        let messages = records
            .iter()
            .flat_map(|record| record_messages(self.deps.ids.as_ref(), record))
            .collect();
        if let Err(reason) = self.commit_record_messages(None, messages).await {
            tracing::warn!(%reason, "the held local command records could not be stored");
        }
    }

    /// 先落库，再进历史、发事件（与别的提交同一个顺序）。`messages` 之后可以接别的消息（`/cd` 的说明文字由调用方
    /// 放在同一批里）。
    pub(crate) async fn commit_record_messages(
        &mut self,
        command: Option<CommandId>,
        messages: Vec<Message>,
    ) -> Result<(), String> {
        let facts = messages
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect();
        self.commit(command, facts).await?;
        for message in messages {
            self.messages.push(message.clone());
            self.log.append(KernelEvent::MessageDone { message });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "local_record_tests.rs"]
mod tests;
