//! 会话标题（docs/specs/rust-cli/titles.md §2、§4）：会话 actor 是标题与标题来源的唯一 owner。
//!
//! 首条人类输入提交后：内存里的标题换成 `first_input`（node-store 在同一事务里按 Node 写好）并发
//! `TitleChanged`；满足触发条件时起一个标题任务，它只算候选标题，结果回到 actor 后才提交
//! `TitleGenerated`（存储按 Node 的来源条件写）、改内存、发事件。用户改名（`Rename`）或 hook 改过标题后，
//! 候选标题丢弃。

use rust_cli_base::{Task, TaskError};
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::session_api::{Ack, KernelEvent};
use rust_cli_kernel_api::store::{Fact, SessionKind};
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, accepted, rejected};
use crate::title_model::long_enough;

/// 标题生成的状态（actor 的一个字段）。
#[derive(Default)]
pub(crate) struct TitleState {
    /// 本 actor 已经尝试过（每个会话最多一次，Node `sessionTitleGenerationAttempted`）。
    attempted: bool,
    /// 标题来源是 `custom`（本会话里改过名或 hook 改过标题）。
    custom: bool,
    /// 在跑的标题任务与首条输入的消息 ID。
    task: Option<(MessageId, Task<Option<String>>)>,
    /// 本 actor 里改名（`Rename`、hook 的标题、起名写入）的次数：起名期间有新的改名时不写
    /// （titles.md §8.2，`superseded`）。
    renames: u64,
}

impl TitleState {
    pub(crate) fn mark_custom(&mut self) {
        self.custom = true;
        self.renames += 1;
    }

    pub(crate) fn renames(&self) -> u64 {
        self.renames
    }
}

/// 标题任务的结果；没有任务时一直等（actor 的 select 分支）。
pub(crate) async fn title_done(
    state: &mut TitleState,
) -> (MessageId, Result<Option<String>, TaskError>) {
    match &mut state.task {
        Some((prompt, task)) => {
            let result = task.await;
            (prompt.clone(), result)
        }
        None => std::future::pending().await,
    }
}

fn human_text(message: &Message) -> Option<String> {
    let Message::User(user) = message else {
        return None;
    };
    if !matches!(user.origin, UserOrigin::Human) {
        return None;
    }
    let texts: Vec<&str> = user
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = texts.join("\n");
    // 用户调用 skill 的输入是命令标签：标题按用户输入的样子（`/<名字> <参数>`，ext.md §2.7）；bash 模式的输入
    // 写成 `! 命令`（首条提示同样还原，tui-input.md §2.2）。
    Some(
        rust_cli_kernel_api::user_command::display(&text)
            .or_else(|| rust_cli_kernel_api::user_input::display(&text))
            .unwrap_or(text),
    )
}

impl Actor {
    /// 本回合的人类输入已提交（`first` 为真时它之前的活动历史里没有 user 消息）。
    pub(crate) fn after_prompt(&mut self, message: &Message, first: bool) {
        let Some(text) = human_text(message) else {
            return;
        };
        if self.meta.title.is_empty() {
            // node-store 在同一事务里写了 first_input（Node `titleFromInput`），这里用同一个函数。
            let title = rust_cli_kernel_api::title::from_input(&text);
            self.meta.title.clone_from(&title);
            self.log.append(KernelEvent::TitleChanged { title });
        }
        let Some(titles) = self.deps.titles.clone() else {
            return;
        };
        let eligible = first
            && !self.titles.attempted
            && !self.titles.custom
            && self.meta.kind == SessionKind::Interactive
            && self.meta.parent.is_none()
            && long_enough(&text);
        if !eligible {
            return;
        }
        self.titles.attempted = true;
        // 入队时冻结当前选择（Node `retainTitleModelContext`）；选择的 owner 是本 actor。
        let selection = self.selection.clone();
        let session = self.meta.id.clone();
        // 提交这条输入的回合（工作线 F64：标题请求与回合共用 `x-query-id`，Node 的标题沿用回合的 trace，model.md §2.4）。
        let run = self
            .running
            .as_ref()
            .and_then(|running| running.run.clone());
        let task = self.deps.spawner.spawn("session-title", async move {
            titles
                .generate(
                    &selection,
                    (&session, run.as_ref()),
                    &text,
                    CancellationToken::new(),
                )
                .await
        });
        self.titles.task = Some((message.id().clone(), task));
    }

    /// 候选标题回到 actor：仍可写时提交、改内存、发事件。
    pub(crate) async fn on_title(
        &mut self,
        prompt: MessageId,
        result: Result<Option<String>, TaskError>,
    ) {
        self.titles.task = None;
        let title = match result {
            Ok(Some(title)) => title,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "the session title task ended without a result");
                return;
            }
        };
        // 用户先改了名，或首条输入已被回退掉（Node `isSuppressedByFirstQueryEdit`）：丢弃。
        if self.titles.custom || !self.messages.iter().any(|m| m.id() == &prompt) {
            tracing::debug!("the generated session title was discarded");
            return;
        }
        let fact = Fact::TitleGenerated {
            title: title.clone(),
            message: Some(prompt),
        };
        if let Err(reason) = self.commit(None, vec![fact]).await {
            // 存储里的来源已不允许覆盖（例如另一个进程改了名）。
            tracing::debug!(%reason, "the generated session title was not stored");
            return;
        }
        self.meta.title.clone_from(&title);
        self.log.append(KernelEvent::TitleChanged { title });
    }

    /// 用户改名（Node `setCustomSessionTitle`）：先提交，再改内存、发事件。
    pub(crate) async fn rename(&mut self, command: CommandId, title: String) -> Ack {
        let fact = Fact::TitleChanged {
            title: title.clone(),
        };
        if let Err(reason) = self.commit(Some(command), vec![fact]).await {
            return rejected(reason);
        }
        self.titles.mark_custom();
        self.meta.title.clone_from(&title);
        self.log.append(KernelEvent::TitleChanged { title });
        accepted()
    }
}

#[cfg(test)]
#[path = "title_tests.rs"]
mod tests;
