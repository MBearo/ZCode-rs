// 复制自 apps/zcode-host/crates/tui/src/ui/mouse.rs（feat/rs 79b6691c55），按 400 行上限拆出绘制部分；F25 改写复制
// 结果的提示并加全屏视图的选区（docs/specs/rust-cli/tui-select.md §2、§3.3）。
//! 鼠标的绘制部分：选区的文字与底色、复制结果、悬停的反馈。
use ratatui::layout::Rect;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::{Point, contains};
use crate::ui::app::App;
use crate::ui::selection::{Row, join, range};
use crate::ui::transcript::Layout;

/// 绘制时处理选区：拖到边缘时把终点移到边缘那一行；端点找不到（行被移走）时清掉选区；算出选中的
/// 文字存进 [`Hits`]。返回选区在全部行里的范围。
pub fn select(
    app: &mut App,
    layout: &Layout,
    start: usize,
    end: usize,
) -> Option<(usize, usize, usize, usize)> {
    let mut selection = app.selection?;
    if selection.dragging && selection.edge != 0 && end > start {
        let edge = if selection.edge > 0 { start } else { end - 1 };
        let (key, offset) = layout.owner(edge);
        selection.head = Point {
            key,
            offset,
            ..selection.head
        };
        app.selection = Some(selection);
    }
    let Some(found) = range(&selection, |p| layout.position(p.key, p.offset)) else {
        app.selection = None;
        return None;
    };
    // 只取选中的行；软折行的续行按记号接到上一行后面（tui-select.md §2.3）。
    let (l1, c1, l2, c2) = found;
    let rows: Vec<Row> = layout
        .lines(l1, l2 + 1)
        .iter()
        .map(|line| Row {
            text: crate::ui::text::plain(line),
            soft: crate::ui::text::soft_wrap(line),
        })
        .collect();
    app.hits.selected = Some(join(&rows, c1, c2));
    Some(found)
}

/// 全屏视图的选区（屏幕坐标）：选中的格子换成选区底色，文字取自这一帧的屏幕格子。在视图画完之后
/// 调用；同时记下每一屏行的文字（双击选词用）。
pub fn paint_screen(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let buffer = frame.buffer_mut();
    let area = buffer.area;
    let row_text = |buffer: &ratatui::buffer::Buffer, y: u16| {
        let mut text = String::new();
        let mut x = area.left();
        while x < area.right() {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        text
    };
    app.hits.screen_rows = (area.top()..area.bottom())
        .map(|y| row_text(buffer, y))
        .collect();
    let Some(selection) = app.screen_selection else {
        return;
    };
    let (start, end) =
        if (selection.anchor.1, selection.anchor.0) <= (selection.head.1, selection.head.0) {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
    let last_row = area.bottom().saturating_sub(1);
    let (r1, r2) = (start.1.min(last_row), end.1.min(last_row));
    let bg = app.palette.selection_bg;
    for y in r1..=r2 {
        let from = if y == r1 { start.0 } else { area.left() };
        let to = if y == r2 {
            end.0.min(area.right().saturating_sub(1))
        } else {
            area.right().saturating_sub(1)
        };
        for x in from..=to {
            buffer[(x, y)].set_bg(bg);
        }
    }
    let rows: Vec<Row> = (r1..=r2)
        .map(|y| Row {
            text: app.hits.screen_rows[usize::from(y)].clone(),
            soft: app
                .hits
                .soft
                .iter()
                .find(|(row, _)| *row == y)
                .map(|(_, soft)| *soft),
        })
        .collect();
    app.hits.selected = Some(join(&rows, usize::from(start.0), usize::from(end.0) + 1));
}

/// 复制结果的提示，显示在 gap 行右侧；放不下时截断加 `…`。
pub fn draw_copied(frame: &mut ratatui::Frame<'_>, app: &App, gap: Rect) {
    let Some((notice, at)) = &app.copied else {
        return;
    };
    if at.elapsed() >= notice.duration || gap.height == 0 {
        return;
    }
    let width = usize::from(gap.width).saturating_sub(2);
    if width < 2 {
        return;
    }
    let label = crate::ui::text::truncate(&notice.text, width);
    let w = label.width() as u16;
    let target = Rect::new(gap.right().saturating_sub(w + 2), gap.y, w, 1);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(Span::styled(
            label,
            ratatui::style::Style::default().fg(app.palette.accent),
        )),
        target,
    );
}

/// 悬停的反馈（§8.7）：鼠标所在的可点击区域换成更亮一档的底色；16 色终端加下划线。在每一帧记下
/// 全部可点击区域之后调用。
pub fn paint_hover(frame: &mut ratatui::Frame<'_>, app: &App) {
    let Some((x, y)) = app.hover else {
        return;
    };
    let Some((rect, _)) = app
        .hits
        .regions
        .iter()
        .rev()
        // 背景标志是彩蛋：可以点击，悬停时不变色（tui.md §7.3）。
        .filter(|(_, target)| *target != super::Target::Logo)
        .find(|(rect, _)| contains(*rect, x, y))
    else {
        return;
    };
    let style = match app.palette.hover_bg {
        Some(bg) => ratatui::style::Style::default().bg(bg),
        None => ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::UNDERLINED),
    };
    let area = frame.area().intersection(*rect);
    frame.buffer_mut().set_style(area, style);
}
