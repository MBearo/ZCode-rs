//! `/goal` 面板与 `/fork` 的会话操作（docs/specs/rust-cli/goal.md §6.2、session-lifecycle.md §6.5.3）：取会话
//! actor 的快照，结果作为 [`Done::ForkGoal`] 回到事件循环。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::goal::GoalView;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::session_api::{Command, KernelError, StartSource};
use serde_json::Value;

use super::Loop;
use super::session::{Done, Sent};
use crate::ui::app::{Local, Overlay};
use crate::ui::fork::{self, Entry};
use crate::ui::goal;
use crate::ui::i18n;
use crate::ui::session_rows::user_text;

/// 异步操作的结果。
pub(super) enum ForkGoalDone {
    Goal(Result<GoalView, KernelError>),
    /// 可选的用户输入（旧的在前）与它们的文字。
    Inputs(Result<Vec<Entry>, KernelError>),
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
            ForkGoalDone::Inputs(Ok(entries)) => self.app.open_fork(entries),
            ForkGoalDone::Goal(Err(error)) | ForkGoalDone::Inputs(Err(error)) => {
                self.app.error(i18n::rejected(lang, &error.to_string()));
            }
        }
    }

    /// `/fork`：可选的用户输入来自会话 actor（与 `/rewind` 同一份，改动统计不用），文字取视图里的消息；
    /// 还没有会话时列表为空。
    pub(super) fn open_fork(&mut self) {
        let Some(session) = self.session.clone() else {
            self.app.open_fork(Vec::new());
            return;
        };
        self.pending.push(
            async move {
                let entries = async {
                    let options = session.rewind_options().await?;
                    let view = session.view().await?;
                    Ok(options
                        .into_iter()
                        .map(|option| {
                            let text = view
                                .messages
                                .iter()
                                .find(|m| m.id() == &option.message)
                                .and_then(user_text)
                                .unwrap_or_default();
                            Entry {
                                message: option.message,
                                text,
                            }
                        })
                        .collect())
                };
                Done::ForkGoal(ForkGoalDone::Inputs(entries.await))
            }
            .boxed(),
        );
    }

    /// 选中一条输入：发 `ForkFromInput`（选择器只在有会话时有条目）。
    pub(super) fn fork(&mut self, input: MessageId) {
        if self.session.is_none() {
            return;
        }
        let command = Command::ForkFromInput { input };
        self.send(Sent::Fork, command, String::new());
    }

    /// 分叉完成：切到新会话，显示回执与切换的提示（随界面语言，`fork::forked_notice`）。
    pub(super) fn forked(&mut self, result: &Value) {
        let Some(id) = result["session"].as_str() else {
            return;
        };
        let notice = fork::forked_notice(self.app.lang, id, result);
        self.switch(SessionId::new(id), StartSource::Fork, Some(notice), true);
    }
}
