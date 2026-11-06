//! 任务清单（Task v2，docs/specs/rust-cli/tui-display.md §2.2）：任务行里的计数与任务卡的 Todos（tui-tasks.md）。清单本身是
//! 内核 `TasksChanged` 的快照（在视图里），这里只记清单是不是最新的回合写的。步骤按内核的次序（Puffin 同）；全部的清单也在
//! ^O 里，挂在最新的任务调用下面（[`steps`]，`cells/work.rs`）。
//!
//! 旧界面多出的（tui-kit.md §4.3）：被没完成的任务阻塞的步骤在文字后面接 ` › blocked by #1, #3`。2026-10-31（用户决定）之前
//! 输入框上方是 Puffin 的待办坞（kit `widgets::todo_dock`，至多 5 行），之前 ctrl+t 收起与展开它；现在任务行只数步骤，^T 打开
//! 任务卡。
use rust_cli_kernel_api::agents::{TaskRecord, TaskStatus};
use rust_cli_tui_kit::app::widgets::tools::Steps;

use super::app::App;
use super::i18n::Lang;
use crate::view::{Body, Transcript};

/// 任务清单的界面状态。
#[derive(Debug, Default)]
pub struct TaskPanel {
    /// 清单最近一次（实时）变化时最新的回合（它的回合头的行 id）：还是最新的回合时清单是新的（Puffin `TodoList.fresh`）。
    /// 冷加载的清单没有它。
    changed_in: Option<Option<u64>>,
}

impl TaskPanel {
    /// 新的快照实时到达（`TasksChanged`）：记下它属于哪个回合（回合头是唯一的事实，不设定时器）。
    pub fn changed(&mut self, view: &Transcript) {
        self.changed_in = Some(latest_turn(view));
    }

    /// 清单是最新的回合写的：之后没有开过新回合（Puffin 在回合开始时清掉 `fresh`）。
    fn fresh(&self, view: &Transcript) -> bool {
        self.changed_in == Some(latest_turn(view))
    }
}

impl App {
    /// 回合在跑或在等你回答（Puffin `Session::busy`）。
    pub(super) fn busy(&self) -> bool {
        self.running() || !super::interaction_view::open_asks(self).is_empty()
    }
}

/// 最新的回合头的行 id。
fn latest_turn(view: &Transcript) -> Option<u64> {
    view.rows()
        .iter()
        .rev()
        .find(|row| matches!(row.body, Body::Turn(_)))
        .map(|row| row.id)
}

/// 一步的文字：被没完成的任务阻塞时后面接 ` › blocked by #1, #3`（旧界面多出的，Puffin 的待办没有阻塞）。
fn step_text(task: &TaskRecord, open: &[&str], lang: Lang) -> String {
    let mut blockers: Vec<&String> = task
        .blocked_by
        .iter()
        .filter(|id| open.contains(&id.as_str()))
        .collect();
    let subject = task.subject.replace(['\n', '\r'], " ");
    if blockers.is_empty() {
        return subject;
    }
    blockers.sort_by(|l, r| match (l.parse::<u64>(), r.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => l.cmp(r),
    });
    let ids = blockers
        .iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(", ");
    match lang {
        Lang::En => format!("{subject} › blocked by {ids}"),
        Lang::Zh => format!("{subject} › 被 {ids} 阻塞"),
    }
}

/// 清单的每一步：状态与文字（kit 的形状；坞与 ^O 里挂在任务调用下面的清单共用）。
pub fn steps(tasks: &[TaskRecord], lang: Lang) -> Steps {
    let open: Vec<&str> = tasks
        .iter()
        .filter(|t| t.status != TaskStatus::Completed)
        .map(|t| t.id.as_str())
        .collect();
    tasks
        .iter()
        .map(|task| {
            let status = match task.status {
                TaskStatus::Pending => "pending",
                TaskStatus::InProgress => "inProgress",
                TaskStatus::Completed => "completed",
            };
            (status.to_owned(), step_text(task, &open, lang))
        })
        .collect()
}

/// 任务行里待办的完成数与总数（tui-tasks.md §2.1）：以前的待办坞此刻会显示时才有（Puffin「待办」的规则：全部完成时回合还在
/// 跑、清单是这个回合写的才说；没全部完成而你发了下一条消息、那个回合没碰它时离开）。2026-10-31 之前这里画坞的行
/// （kit `todo_dock`，至多 5 行）。
pub fn todo_counts(app: &App) -> Option<(usize, usize)> {
    let tasks = app.view.tasks();
    if tasks.is_empty() {
        return None;
    }
    let total = tasks.len();
    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let (busy, fresh) = (app.busy(), app.snapshots.tasks.fresh(&app.view));
    let shown = if done == total {
        busy && fresh
    } else {
        busy || fresh
    };
    shown.then_some((done, total))
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
