//! 任务清单（tui-display.md §2.2；tui-tasks.md §2.1、§3.2）：清单整理成 kit 的形状、阻塞的说明、中文的字、任务行的数。
use rust_cli_kernel_api::agents::{TaskRecord, TaskStatus};

use super::*;

fn task(id: &str, status: TaskStatus, blocked_by: &[&str]) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        subject: format!("Task {id}"),
        description: String::new(),
        active_form: None,
        owner: None,
        status,
        blocks: Vec::new(),
        blocked_by: blocked_by.iter().map(|id| (*id).to_owned()).collect(),
        metadata: None,
    }
}

/// 步骤按内核的次序，状态写成 kit 的词；被没完成的任务阻塞的接 ` › blocked by`（已完成的阻塞不算）。
#[test]
fn tasks_become_dock_steps_in_kernel_order() {
    let tasks = [
        task("2", TaskStatus::Completed, &[]),
        task("1", TaskStatus::InProgress, &[]),
        task("4", TaskStatus::Pending, &["3", "1", "2"]),
        task("3", TaskStatus::Pending, &[]),
    ];
    let list = steps(&tasks, Lang::En);
    let steps: Vec<(&str, &str)> = list.iter().map(|(s, t)| (s.as_str(), t.as_str())).collect();
    assert_eq!(
        steps,
        [
            ("completed", "Task 2"),
            ("inProgress", "Task 1"),
            ("pending", "Task 4 › blocked by #1, #3"),
            ("pending", "Task 3"),
        ]
    );
    let zh = super::steps(&tasks, Lang::Zh);
    assert_eq!(zh[2].1, "Task 4 › 被 #1, #3 阻塞");
}

/// 任务行的待办数（tui-tasks.md §2.1）同以前的坞的规则：没全部完成而清单是这个回合写的时有；全部完成时只在回合还在跑时有。
#[test]
fn todo_counts_follow_the_old_dock_rule() {
    let mut app = crate::ui::task_line::tests::app(Lang::En);
    assert_eq!(todo_counts(&app), None);
    crate::ui::task_line::tests::todos(&mut app, 2, 3);
    assert_eq!(todo_counts(&app), Some((2, 3)));
    crate::ui::task_line::tests::todos(&mut app, 3, 3);
    assert_eq!(todo_counts(&app), None);
}
