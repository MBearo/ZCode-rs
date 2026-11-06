//! 待办清单的一步（docs/puffin-tui.md §4.2「待办」）：标记、一个空格与文字。`Todos` 调用的证据与待办的坞都这样画。
//! 移植自 Puffin 的 shell/todo_dock.rs 的 `Status`、`status`、`step`。
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

/// 一步的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Done,
    Now,
    Todo,
}

/// 状态的文字（`completed`、`inProgress` / `in_progress`、其余）读成 [`Status`]。
pub fn status(s: &str) -> Status {
    match s {
        "completed" => Status::Done,
        "inProgress" | "in_progress" => Status::Now,
        _ => Status::Todo,
    }
}

/// 一步：它的标记、一个空格与它的文字。`quiet` 是停在那里的清单：它的 `○` 不是 agent 此刻在做的。
pub fn step(theme: &Theme, st: Status, text: &str, quiet: bool) -> Vec<Span> {
    let (mark, ink) = match st {
        Status::Done => (Span::new("✓", Style::fg(theme.ok)), theme.fg3),
        Status::Now if quiet => (Span::new("○", theme.line), theme.fg2),
        Status::Now => (Span::new("○", theme.fg3), Style::DEFAULT),
        Status::Todo => (Span::new("·", theme.fg3), theme.fg2),
    };
    vec![
        mark,
        Span::new(" ", Style::DEFAULT),
        Span::new(text.to_owned(), ink),
    ]
}
