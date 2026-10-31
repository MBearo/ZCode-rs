//! 不带参数的 `/rename`：模型起名（docs/specs/rust-cli/titles.md §8.2）。会话 actor 是标题与来源的唯一
//! owner：对话文本在 actor 里取，起名任务只算名字，回到 actor 后才按 `Rename` 提交、发事件、回执。
//! 同一时间只有一个起名任务；起名期间有新的改名（`Rename` 或 hook）时不写，回执 `superseded` 的文本。

use futures_util::StreamExt;
use rust_cli_base::{Task, TaskError};
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent, small_fast_model};
use rust_cli_kernel_api::session_api::Ack;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, rejected};
use crate::rename_text::{clean_name, conversation, parse_name, request};

/// 没有对话与解析失败用同一段文本。
const NO_NAME: &str =
    "Could not generate a name: no conversation context yet. Usage: /rename <name>";
const BUSY: &str = "A session name is being generated.";

/// 在跑的起名任务（actor 的 `naming` 字段）。
pub(crate) struct NameJob {
    command: CommandId,
    reply: oneshot::Sender<Ack>,
    /// 起名开始时的改名计数（`TitleState::renames`）。
    renames: u64,
    task: Task<Option<String>>,
}

/// 起名任务的结果；没有任务时一直等（actor 的 select 分支）。
pub(crate) async fn name_done(job: &mut Option<NameJob>) -> Result<Option<String>, TaskError> {
    match job {
        Some(job) => (&mut job.task).await,
        None => std::future::pending().await,
    }
}

impl Actor {
    /// `GenerateName`：回执等名字（与代码回退相同，回执晚于命令）。
    pub(crate) fn generate_name(&mut self, command: CommandId, reply: oneshot::Sender<Ack>) {
        if self.naming.is_some() {
            let _ = reply.send(rejected(BUSY));
            return;
        }
        let text = conversation(&self.messages);
        if text.is_empty() {
            let _ = reply.send(rejected(NO_NAME));
            return;
        }
        // 模型经小模型槽位，输入是 actor 的当前选择（model.md §6.1）。
        let selection = small_fast_model(&self.selection);
        let session = self.meta.id.clone();
        let model = self.deps.side_model.clone();
        let task = self.deps.spawner.spawn("session-name", async move {
            // 目录的输出上限：借卡片形状的 GLM 按它（工作线 F121）。
            let configured = model
                .capabilities(&selection)
                .await
                .and_then(|found| found.max_output_tokens);
            let request = request(&selection, &session, (&text, configured));
            let mut stream = model.stream(request, CancellationToken::new());
            let mut reply = String::new();
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ModelEvent::BlockStart {
                        block: ContentBlock::Text { text },
                        ..
                    }) => reply.push_str(&text),
                    Ok(ModelEvent::BlockDelta {
                        delta: BlockDelta::Text(delta),
                        ..
                    }) => reply.push_str(&delta),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "session name generation failed");
                        return None;
                    }
                }
            }
            parse_name(&reply).as_deref().and_then(clean_name)
        });
        self.naming = Some(NameJob {
            command,
            reply,
            renames: self.titles.renames(),
            task,
        });
    }

    /// 名字回到 actor：起名期间没有新的改名时按 `Rename` 写入。
    pub(crate) async fn on_name(&mut self, result: Result<Option<String>, TaskError>) {
        let Some(job) = self.naming.take() else {
            return;
        };
        let name = match result {
            Ok(Some(name)) => name,
            Ok(None) => {
                let _ = job.reply.send(rejected(NO_NAME));
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "the session name task ended without a result");
                let _ = job.reply.send(rejected(NO_NAME));
                return;
            }
        };
        let ack = if self.titles.renames() == job.renames {
            match self.rename(job.command, name.clone()).await {
                Ack::Accepted { .. } => Ack::Accepted {
                    result: json!({"name": name, "message": format!("Session renamed to: {name}")}),
                },
                other => other,
            }
        } else {
            // `superseded`：新的改名先落地，保留它。
            let current = self.meta.title.clone();
            Ack::Accepted {
                result: json!({
                    "name": current,
                    "message": format!("Session is named: {current} (a newer rename landed first)"),
                }),
            }
        };
        let _ = job.reply.send(ack);
    }
}

#[cfg(test)]
#[path = "rename_tests.rs"]
mod tests;
