//! 只读页面的翻页键与列表的循环移动（架构整理 U12）：以前独立页面、详细视图与 `/resume` 的全屏对话各写一份翻页键，
//! 循环移动（到头绕回）写了 6 份。
use crossterm::event::{KeyCode, KeyEvent};

/// 列表里循环移动一格：到头绕回；没有条目时停在 0。
pub fn cycle(index: usize, count: usize, down: bool) -> usize {
    match count {
        0 => 0,
        _ if down => (index + 1) % count,
        _ => (index + count - 1) % count,
    }
}

/// 翻页键的含义（三处共有的键；各页另有的键，如 `End`、`g`、`b`，由各页自己处理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// 往下（正）或往上（负）几行：↓ / `j`、↑ / `k`。
    Lines(isize),
    /// 往下或往上一页：PgDn / 空格、PgUp。
    Pages(isize),
    /// 回到顶上：Home。
    Top,
}

pub fn page_move(key: &KeyEvent) -> Option<Move> {
    Some(match key.code {
        KeyCode::Down | KeyCode::Char('j') => Move::Lines(1),
        KeyCode::Up | KeyCode::Char('k') => Move::Lines(-1),
        KeyCode::PageDown | KeyCode::Char(' ') => Move::Pages(1),
        KeyCode::PageUp => Move::Pages(-1),
        KeyCode::Home => Move::Top,
        _ => return None,
    })
}

/// 从顶上量的滚动量按翻页键的含义移动（`page` 是一页的行数）。
pub fn scroll_from_top(scroll: &mut usize, step: Move, page: usize) {
    match step {
        Move::Lines(lines) => *scroll = scroll.saturating_add_signed(lines),
        Move::Pages(pages) => *scroll = scroll.saturating_add_signed(pages * page as isize),
        Move::Top => *scroll = 0,
    }
}
