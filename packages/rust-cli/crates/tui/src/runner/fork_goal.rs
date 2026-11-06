//! `/goal` 的卡与 `/fork` 的会话操作（docs/specs/rust-cli/goal.md §6.2、session-lifecycle.md §6.5.3）：取会话
//! actor 的快照，结果作为 [`Done::ForkGoal`] 回到事件循环。
use futures_util::FutureExt;
use rust_cli_kernel_api::goal::GoalView;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::session_api::{Command, Forked, KernelError, StartSource};

use super::Loop;
use super::session::{Done, Sent};
use crate::ui::fork::{self, Entry};
use crate::ui::i18n;
use crate::ui::session_rows::user_text;

/// 异步操作的结果。
pub(super) enum ForkGoalDone {
    Goal(Result<GoalView, KernelError>),
    /// 重取的目标与取它的会话（cards.rs）：换了会话之后到的作废。
    Refreshed(SessionId, Result<GoalView, KernelError>),
    /// 可选的用户输入（旧的在前）与它们的文字。
    Inputs(Result<Vec<Entry>, KernelError>),
}

impl Loop<'_> {
    /// `/goal` 不带参数：还没有会话时没有目标。
    pub(super) fn show_goal(&mut self) {
        let Some(session) = self.outbox.session().cloned() else {
            self.open_goal(&GoalView::default(), true);
            return;
        };
        self.pending
            .push(async move { Done::ForkGoal(ForkGoalDone::Goal(session.goal().await)) }.boxed());
    }

    /// 目标的卡（cards.rs）：`opening` 是 `/goal` 刚发出的；否则只更新开着的卡（目标的检查到了，Puffin 的 `wants.goal`）。
    fn open_goal(&mut self, view: &GoalView, opening: bool) {
        let now = self.now_ms();
        self.app.open_goal_card(view, now, opening);
    }

    /// 重取会话的目标（状态栏的记号、开着的卡）：接上会话时，与目标的检查到了、目标变了时（Puffin fold 的 `goal_status`
    /// 之后的 `wants.goal`）。
    pub(super) fn refresh_goal(&mut self) {
        let Some(session) = self.outbox.session().cloned() else {
            return;
        };
        let id = session.id().clone();
        self.pending.push(
            async move { Done::ForkGoal(ForkGoalDone::Refreshed(id, session.goal().await)) }
                .boxed(),
        );
    }

    pub(super) fn on_fork_goal(&mut self, done: ForkGoalDone) {
        let lang = self.app.lang;
        match done {
            ForkGoalDone::Goal(Ok(view)) => self.open_goal(&view, true),
            ForkGoalDone::Refreshed(id, Ok(view))
                if self.outbox.session().is_some_and(|s| *s.id() == id) =>
            {
                self.open_goal(&view, false)
            }
            // 重取失败或会话已经换了：留着上一次的目标。
            ForkGoalDone::Refreshed(..) => {}
            ForkGoalDone::Inputs(Ok(entries)) => self.app.open_fork(entries),
            ForkGoalDone::Goal(Err(error)) | ForkGoalDone::Inputs(Err(error)) => {
                self.app.error(i18n::rejected(lang, &error.to_string()));
            }
        }
    }

    /// `/fork`：可选的用户输入来自会话 actor（与 `/rewind` 同一份，改动统计不用），文字取视图里的消息；
    /// 还没有会话时列表为空。
    pub(super) fn open_fork(&mut self) {
        let Some(session) = self.outbox.session().cloned() else {
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
        if self.outbox.session().is_none() {
            return;
        }
        let command = Command::ForkFromInput { input };
        self.send(Sent::Fork, command);
    }

    /// 分叉完成：切到新会话，显示回执与切换的提示（随界面语言，`fork::forked_notice`）。
    pub(super) fn forked(&mut self, forked: &Forked) {
        let notice = fork::forked_notice(self.app.lang, forked);
        self.switch(
            forked.session.clone(),
            StartSource::Fork,
            Some(notice),
            true,
        );
    }
}
