//! 共用的文字样式与面板头的线（架构整理 U11：以前 `dim` 各文件写了 11 份、`bold` 3 份，`Palette::dim` 又是一份；
//! 面板头整行的 `─` 写了 14 处）。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// 次要的文字：dim。
pub fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// 标题与强调：粗体。
pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// 面板头的线：`width` 个 `─`，用 `style`（通常是强调色）。
pub fn rule(width: usize, style: Style) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), style))
}
