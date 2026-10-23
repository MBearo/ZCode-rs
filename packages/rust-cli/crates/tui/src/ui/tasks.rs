//! 任务清单（Task v2，docs/specs/rust-cli/tui-display.md §2.2）：输入框上方的独立清单，ctrl+t 展开与收起。
//! 清单本身是内核 `TasksChanged` 的快照（在视图里），
//! 这里只有界面状态：是否展开、每项第一次被看到完成的时刻（排序用）。
use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::agents::{TaskRecord, TaskStatus};

use super::app::App;
use super::i18n::Lang;
use super::text::truncate;
use crate::view::task_counts;

/// 刚完成的任务在截断时优先显示的时长。
const RECENT: Duration = Duration::from_secs(30);

/// 任务清单的界面状态。
#[derive(Debug, Default)]
pub struct TaskPanel {
    /// 展开（显示任务清单）。
    pub expanded: bool,
    /// 任务 ID → 第一次看到它完成的时刻；冷加载或第一次快照里已完成的记 None（不算“刚完成”）。
    completed: HashMap<String, Option<Instant>>,
    seen_any: bool,
}

impl TaskPanel {
    /// 新的快照到达（`TasksChanged`）：展开（每次 TaskCreate / TaskUpdate 都展开），
    /// 记下新完成的任务。
    pub fn changed(&mut self, tasks: &[TaskRecord], now: Instant) {
        self.expanded = true;
        let first = !self.seen_any;
        self.seen_any = true;
        let done: Vec<&str> = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Completed)
            .map(|task| task.id.as_str())
            .collect();
        self.completed.retain(|id, _| done.contains(&id.as_str()));
        for id in done {
            self.completed
                .entry(id.to_owned())
                .or_insert((!first).then_some(now));
        }
    }

    fn recent(&self, id: &str, now: Instant) -> bool {
        self.completed
            .get(id)
            .copied()
            .flatten()
            .is_some_and(|at| now.duration_since(at) < RECENT)
    }
}

/// 按 ID 的数值排序（`parseInt(a) - parseInt(b)`），不是数字时按字符串。
fn by_id(left: &TaskRecord, right: &TaskRecord) -> std::cmp::Ordering {
    match (left.id.parse::<u64>(), right.id.parse::<u64>()) {
        (Ok(l), Ok(r)) => l.cmp(&r),
        _ => left.id.cmp(&right.id),
    }
}

/// 最多显示的行数：`rows <= 10 ? 0 : min(5, max(3, rows - 14))`。
fn max_display(rows: usize) -> usize {
    if rows <= 10 {
        0
    } else {
        rows.saturating_sub(14).clamp(3, 5)
    }
}

fn header(tasks: &[TaskRecord], lang: Lang) -> Line<'static> {
    let (done, progress, open) = task_counts(tasks);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = dim.add_modifier(Modifier::BOLD);
    let n = |value: usize| Span::styled(value.to_string(), bold);
    let t = |text: &str| Span::styled(text.to_owned(), dim);
    let mut spans = vec![Span::raw("  ")];
    match lang {
        Lang::En => {
            spans.extend([n(tasks.len()), t(" tasks ("), n(done), t(" done, ")]);
            if progress > 0 {
                spans.extend([n(progress), t(" in progress, ")]);
            }
            spans.extend([n(open), t(" open)")]);
        }
        Lang::Zh => {
            spans.extend([n(tasks.len()), t(" 个任务（"), n(done), t(" 完成，")]);
            if progress > 0 {
                spans.extend([n(progress), t(" 进行中，")]);
            }
            spans.extend([n(open), t(" 待办）")]);
        }
    }
    Line::from(spans)
}

fn item(
    task: &TaskRecord,
    open: &[&str],
    accent: Color,
    width: usize,
    lang: Lang,
) -> Line<'static> {
    let mut blockers: Vec<&String> = task
        .blocked_by
        .iter()
        .filter(|id| open.contains(&id.as_str()))
        .collect();
    blockers.sort_by(|l, r| match (l.parse::<u64>(), r.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => l.cmp(r),
    });
    let blocked = !blockers.is_empty();
    let (icon, icon_style) = match task.status {
        TaskStatus::Completed => ("✔", Style::default().fg(Color::Green)),
        TaskStatus::InProgress => ("◼", Style::default().fg(accent)),
        TaskStatus::Pending => ("◻", Style::default()),
    };
    let mut subject = Style::default();
    match task.status {
        TaskStatus::Completed => {
            subject = subject.add_modifier(Modifier::CROSSED_OUT | Modifier::DIM);
        }
        TaskStatus::InProgress => subject = subject.add_modifier(Modifier::BOLD),
        TaskStatus::Pending => {}
    }
    if blocked {
        subject = subject.add_modifier(Modifier::DIM);
    }
    // 标题最多 `columns - 15` 列（不少于 15）。
    let shown = truncate(&task.subject, width.saturating_sub(15).max(15));
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{icon} "), icon_style),
        Span::styled(shown, subject),
    ];
    if blocked {
        let ids = blockers
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        let text = match lang {
            Lang::En => format!(" › blocked by {ids}"),
            Lang::Zh => format!(" › 被 {ids} 阻塞"),
        };
        spans.push(Span::styled(
            text,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// 放不下时的顺序：刚完成、进行中、待办（没被阻塞的在前）、较早完成；同组按 ID。
fn ordered<'a>(
    tasks: &'a [TaskRecord],
    panel: &TaskPanel,
    open: &[&str],
    now: Instant,
) -> Vec<&'a TaskRecord> {
    let sorted = |filter: &dyn Fn(&TaskRecord) -> bool| {
        let mut group: Vec<&TaskRecord> = tasks.iter().filter(|t| filter(t)).collect();
        group.sort_by(|l, r| by_id(l, r));
        group
    };
    let done = |t: &TaskRecord| t.status == TaskStatus::Completed;
    let blocked = |t: &TaskRecord| t.blocked_by.iter().any(|id| open.contains(&id.as_str()));
    let mut pending = sorted(&|t| t.status == TaskStatus::Pending);
    pending.sort_by(|l, r| blocked(l).cmp(&blocked(r)).then_with(|| by_id(l, r)));
    let mut all = sorted(&|t| done(t) && panel.recent(&t.id, now));
    all.extend(sorted(&|t| t.status == TaskStatus::InProgress));
    all.extend(pending);
    all.extend(sorted(&|t| done(t) && !panel.recent(&t.id, now)));
    all
}

fn hidden_summary(hidden: &[&TaskRecord], lang: Lang) -> String {
    let count = |status: TaskStatus| hidden.iter().filter(|t| t.status == status).count();
    let parts: Vec<String> = [
        (count(TaskStatus::InProgress), "in progress", "进行中"),
        (count(TaskStatus::Pending), "pending", "待办"),
        (count(TaskStatus::Completed), "completed", "完成"),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, en, zh)| match lang {
        Lang::En => format!("{n} {en}"),
        Lang::Zh => format!("{n} {zh}"),
    })
    .collect();
    let joiner = match lang {
        Lang::En => ", ",
        Lang::Zh => "，",
    };
    format!("  … +{}", parts.join(joiner))
}

/// 输入框上方的清单；没有任务或收起时为空。`rows` 是终端高度。
pub fn lines(app: &App, width: usize, rows: usize, now: Instant) -> Vec<Line<'static>> {
    let tasks = app.view.tasks();
    if !app.tasks.expanded || tasks.is_empty() {
        return Vec::new();
    }
    let open: Vec<&str> = tasks
        .iter()
        .filter(|t| t.status != TaskStatus::Completed)
        .map(|t| t.id.as_str())
        .collect();
    let limit = max_display(rows);
    let mut out = vec![header(tasks, app.lang)];
    let (visible, hidden): (Vec<&TaskRecord>, Vec<&TaskRecord>) = if tasks.len() > limit {
        let all = ordered(tasks, &app.tasks, &open, now);
        let (shown, rest) = all.split_at(limit.min(all.len()));
        (shown.to_vec(), rest.to_vec())
    } else {
        let mut all: Vec<&TaskRecord> = tasks.iter().collect();
        all.sort_by(|l, r| by_id(l, r));
        (all, Vec::new())
    };
    out.extend(
        visible
            .iter()
            .map(|task| item(task, &open, app.palette.accent, width, app.lang)),
    );
    if limit > 0 && !hidden.is_empty() {
        out.push(Line::from(Span::styled(
            hidden_summary(&hidden, app.lang),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    out
}

/// footer 的提示：有任务时 `ctrl+t to hide tasks` / `ctrl+t to show tasks`。
pub fn toggle_hint(app: &App) -> Option<&'static str> {
    if app.view.tasks().is_empty() {
        return None;
    }
    Some(match (app.lang, app.tasks.expanded) {
        (Lang::En, true) => "{ctrl+t} to hide tasks",
        (Lang::En, false) => "{ctrl+t} to show tasks",
        (Lang::Zh, true) => "{ctrl+t} 隐藏任务",
        (Lang::Zh, false) => "{ctrl+t} 显示任务",
    })
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
