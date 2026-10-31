//! 选择列表的一行（docs/specs/rust-cli/tui.md §7.4，修复线 F26，用户决定）：当前行前面 `› `，整行铺 `selection`
//! 底色条，名字粗体，说明不 dim；其余行前面两个空格，说明用 `secondary`。所有上下选择的列表都用它画当前行
//! （提问视图的选项除外：不铺底色条，tui.md §7.6，工作线 F74），选中的样式只在这里与 `Palette::selection`。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::palette::Palette;

/// 当前行的指针（U+203A）。
pub const POINTER: &str = "› ";
/// 其余行的占位。
pub const BLANK: &str = "  ";

pub fn pointer(selected: bool) -> Span<'static> {
    Span::raw(if selected { POINTER } else { BLANK })
}

/// 一段是不是说明一类的次要文字（dim，或用了 `secondary` 的颜色）：在底色条上不加粗。
fn is_secondary(style: Style, secondary: Style) -> bool {
    style.add_modifier.contains(Modifier::DIM)
        || (secondary.fg.is_some() && style.fg == secondary.fg)
}

/// 选中时把整行画成底色条并补到 `width`；没选中时原样返回。
pub fn paint(
    mut line: Line<'static>,
    selected: bool,
    palette: &Palette,
    width: usize,
) -> Line<'static> {
    if !selected {
        return line;
    }
    let bar = palette.selection();
    let secondary = palette.secondary();
    let reversed = bar.add_modifier.contains(Modifier::REVERSED);
    for span in &mut line.spans {
        let quiet = is_secondary(span.style, secondary);
        let mut style = if reversed {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
                .fg(bar.fg.unwrap_or_default())
                .bg(bar.bg.unwrap_or_default())
        };
        // 保留斜体、删除线这类不影响读的修饰；名字粗体，说明不加粗、不 dim。
        style.add_modifier |= span.style.add_modifier & (Modifier::ITALIC | Modifier::CROSSED_OUT);
        if !quiet {
            style = style.add_modifier(Modifier::BOLD);
        }
        span.style = style.remove_modifier(Modifier::DIM);
    }
    let used = line.width();
    if used < width {
        let fill = if reversed {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(bar.bg.unwrap_or_default())
        };
        line.spans
            .push(Span::styled(" ".repeat(width - used), fill));
    }
    line.style = if reversed {
        Style::default()
    } else {
        Style::default().bg(bar.bg.unwrap_or_default())
    };
    line
}

#[cfg(test)]
#[path = "pick_tests.rs"]
mod tests;
