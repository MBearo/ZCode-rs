//! 任务清单的排版（tui-display.md §2.2）：标题、图标、阻塞、截断的顺序与隐藏的汇总。
use std::time::{Duration, Instant};

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

fn plain(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(crate::ui::text::plain).collect()
}

#[test]
fn the_display_limit_follows_the_terminal_height() {
    assert_eq!(max_display(10), 0);
    assert_eq!(max_display(16), 3);
    assert_eq!(max_display(18), 4);
    assert_eq!(max_display(40), 5);
}

#[test]
fn the_header_counts_done_in_progress_and_open() {
    let tasks = [
        task("1", TaskStatus::Completed, &[]),
        task("2", TaskStatus::InProgress, &[]),
        task("3", TaskStatus::Pending, &[]),
    ];
    let line = crate::ui::text::plain(&header(&tasks, Lang::En));
    assert_eq!(line, "  3 tasks (1 done, 1 in progress, 1 open)");
    let tasks = [task("1", TaskStatus::Pending, &[])];
    let line = crate::ui::text::plain(&header(&tasks, Lang::En));
    assert_eq!(line, "  1 tasks (0 done, 1 open)");
    let line = crate::ui::text::plain(&header(&tasks, Lang::Zh));
    assert_eq!(line, "  1 个任务（0 完成，1 待办）");
}

#[test]
fn items_show_icons_and_open_blockers() {
    let open = ["1", "3"];
    let blocked = task("4", TaskStatus::Pending, &["3", "1", "2"]);
    let line = crate::ui::text::plain(&item(&blocked, &open, Color::Cyan, 80, Lang::En));
    assert_eq!(line, "  ◻ Task 4 › blocked by #1, #3");
    let done = task("2", TaskStatus::Completed, &[]);
    let line = item(&done, &open, Color::Cyan, 80, Lang::En);
    assert_eq!(crate::ui::text::plain(&line), "  ✔ Task 2");
    assert!(
        line.spans[2]
            .style
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    let running = item(
        &task("1", TaskStatus::InProgress, &[]),
        &open,
        Color::Cyan,
        80,
        Lang::En,
    );
    assert!(running.spans[2].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(running.spans[1].style.fg, Some(Color::Cyan));
}

#[test]
fn overflow_puts_recent_and_active_tasks_first() {
    let now = Instant::now();
    let tasks = vec![
        task("1", TaskStatus::Completed, &[]),
        task("2", TaskStatus::Completed, &[]),
        task("3", TaskStatus::Pending, &["4"]),
        task("4", TaskStatus::InProgress, &[]),
        task("5", TaskStatus::Pending, &[]),
        task("6", TaskStatus::Pending, &[]),
    ];
    let mut panel = TaskPanel::default();
    // 第一份快照里已完成的不算“刚完成”；之后完成的 2 算。
    panel.changed(&tasks[..1], now - Duration::from_secs(60));
    panel.changed(&tasks, now);
    let open = ["3", "4", "5", "6"];
    let ids: Vec<&str> = ordered(&tasks, &panel, &open, now)
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert_eq!(ids, ["2", "4", "5", "6", "3", "1"]);
    // 30 秒之后 2 也算较早完成。
    let later = now + Duration::from_secs(31);
    let ids: Vec<&str> = ordered(&tasks, &panel, &open, later)
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert_eq!(ids, ["4", "5", "6", "3", "1", "2"]);
    let hidden = [&tasks[0], &tasks[2], &tasks[4]];
    assert_eq!(
        hidden_summary(&hidden, Lang::En),
        "  … +2 pending, 1 completed"
    );
    let _ = plain(&[]);
}
