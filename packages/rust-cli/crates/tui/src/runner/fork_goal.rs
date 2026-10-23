//! `/goal` 面板与 `/fork` 的会话操作（docs/specs/rust-cli/goal.md §6.2、session-lifecycle.md §6.5.4）：取会话
//! actor 的快照，结果作为 [`Done::ForkGoal`] 回到事件循环。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::goal::GoalView;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::{Command, ForkFiles, KernelError, StartSource};
use serde_json::Value;

use super::Loop;
use super::session::{Done, Sent};
use crate::ui::app::{Local, Overlay};
use crate::ui::fork::Entry;
use crate::ui::goal;
use crate::ui::i18n;
use crate::ui::session_rows::user_text;

/// 异步操作的结果。
pub(super) enum ForkGoalDone {
    Goal(Result<GoalView, KernelError>),
    /// checkpoint 清单（新的在前）与回合输入的文字。
    Checkpoints(Result<Vec<Entry>, KernelError>),
}

impl<B: Backend> Loop<'_, B> {
    /// `/goal` 不带参数：还没有会话时没有目标。
    pub(super) fn show_goal(&mut self) {
        let Some(session) = self.session.clone() else {
            self.open_goal(&GoalView::default());
            return;
        };
        self.pending
            .push(async move { Done::ForkGoal(ForkGoalDone::Goal(session.goal().await)) }.boxed());
    }

    fn open_goal(&mut self, view: &GoalView) {
        let panel = goal::panel(view, self.app.lang, self.now_ms());
        self.app.overlay = Some(Overlay::Page {
            local: Local::Goal(Box::new(panel)),
            scroll: 0,
        });
    }

    pub(super) fn on_fork_goal(&mut self, done: ForkGoalDone) {
        let lang = self.app.lang;
        match done {
            ForkGoalDone::Goal(Ok(view)) => self.open_goal(&view),
            ForkGoalDone::Checkpoints(Ok(entries)) => self.app.open_fork(entries, self.now_ms()),
            ForkGoalDone::Goal(Err(error)) | ForkGoalDone::Checkpoints(Err(error)) => {
                self.app.error(i18n::rejected(lang, &error.to_string()));
            }
        }
    }

    /// `/fork` 不带参数：取 checkpoint 清单与视图里回合输入的文字，再打开选择器。
    pub(super) fn open_fork(&mut self) {
        let Some(session) = self.session.clone() else {
            self.app.open_fork(Vec::new(), self.now_ms());
            return;
        };
        self.pending.push(
            async move {
                let entries = async {
                    let checkpoints = session.checkpoints().await?;
                    let view = session.view().await?;
                    Ok(checkpoints
                        .into_iter()
                        .map(|checkpoint| {
                            let text = view
                                .messages
                                .iter()
                                .find(|m| m.id() == &checkpoint.prompt)
                                .and_then(user_text)
                                .unwrap_or_default();
                            Entry { checkpoint, text }
                        })
                        .collect())
                };
                Done::ForkGoal(ForkGoalDone::Checkpoints(entries.await))
            }
            .boxed(),
        );
    }

    /// 发 `ForkCheckpoint`；还没有会话时没有 checkpoint（Node 的原文）。
    pub(super) fn fork_checkpoint(&mut self, checkpoint: Option<String>, files: ForkFiles) {
        if self.session.is_none() {
            self.app
                .info("No workspace checkpoint is available yet.".to_owned());
            return;
        }
        let command = Command::ForkCheckpoint { checkpoint, files };
        self.send(Sent::ForkCheckpoint, command, String::new());
    }

    /// 分叉完成：切到新会话，显示回执与 Node 的 `Switched to forked session <ID>.`。
    pub(super) fn forked(&mut self, result: &Value) {
        let Some(id) = result["session"].as_str() else {
            return;
        };
        let response = result["response"].as_str().unwrap_or_default();
        let notice = format!("{response}\nSwitched to forked session {id}.");
        self.switch(SessionId::new(id), StartSource::Fork, Some(notice));
    }
}
