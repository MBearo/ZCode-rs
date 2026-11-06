// 复制自 apps/zcode-host/crates/tui/src/ui/mouse.rs（feat/rs 79b6691c55），按 400 行上限拆出绘制部分；F25 改写复制
// 结果的提示并加全屏视图的选区（docs/specs/rust-cli/tui-select.md §2、§3.3）。
//! 鼠标的绘制部分：选区的文字与底色、复制结果、悬停的反馈。
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::{Grain, Point};
use crate::ui::app::App;
use crate::ui::frame::{overlay, pad};
use crate::ui::layout;
use crate::ui::selection::{Row, join};
use crate::ui::transcript::Layout;

/// 绘制时处理选区：拖到边缘时把终点移到边缘那一行；端点找不到（行被移走）时清掉选区；算出选中的
/// 文字存进 [`Hits`]。返回选区在全部行里的范围。
pub fn select(
    app: &mut App,
    layout: &Layout,
    start: usize,
    end: usize,
) -> Option<(usize, usize, usize, usize)> {
    let mut selection = app.pointer.selection?;
    if selection.dragging && selection.edge != 0 && end > start {
        let edge = if selection.edge > 0 { start } else { end - 1 };
        let (key, offset) = layout.owner(edge);
        selection.head = Point {
            key,
            offset,
            ..selection.head
        };
        app.pointer.selection = Some(selection);
    }
    let find = |p: &Point| layout.position(p.key, p.offset).map(|line| (line, p.col));
    let (Some(anchor), Some(head)) = (find(&selection.anchor), find(&selection.head)) else {
        app.pointer.selection = None;
        return None;
    };
    // 两处各按单位取，并起来（Puffin `press` 的 `base` 加 `drag` 指针下同一单位的）；格的选区两端相同时是空的。
    let base = span(layout, anchor, selection.grain);
    let under = span(layout, head, selection.grain);
    if selection.grain == Grain::Cell && anchor == head {
        return None;
    }
    let (from, to) = (base.0.min(under.0), base.1.max(under.1));
    // 复制的字同 Puffin（kit `select_units::copy_text`，tui-select.md §2.3）：软折行的续行接回上一行，板边的行不要，
    // 去掉行尾的空白与共同的缩进。以前用旧界面自己的 `join`，按 Unicode 去行尾空白：流式回答里还没显示的字母占位的
    // 不换行空格也被去掉，复制的字数与 Puffin 不同（孪生 copy-feedback）。
    app.pointer.hits.selected = Some(rust_cli_tui_kit::app::select_units::copy_text(
        &layout.styled_lines(from.0, to.0 + 1),
        from.1,
        to.1,
    ));
    Some((from.0, from.1, to.0, to.1 + 1))
}

/// `p`（行，列）处 `grain` 的单位，第一格与最后一格（Puffin `span_at`）：格是它自己；词与逻辑行按 kit 的
/// `select_units`，跨过软折行；那里没有字时是它自己。
fn span(layout: &Layout, p: (usize, usize), grain: Grain) -> ((usize, usize), (usize, usize)) {
    use rust_cli_tui_kit::app::select_units::{line_at, logical, word_at};
    let glyphs = || {
        // 软折行不越过块：块的第一行没有 `cont`。
        let (mut a, mut b) = (p.0, p.0);
        while a > 0 && layout.styled(a).cont.is_some() {
            a -= 1;
        }
        while b + 1 < layout.len() && layout.styled(b + 1).cont.is_some() {
            b += 1;
        }
        logical(&layout.styled_lines(a, b + 1), p.0 - a, a)
    };
    let found = match grain {
        Grain::Cell => None,
        Grain::Word => word_at(&glyphs(), p.0, p.1),
        Grain::Line => line_at(&glyphs()),
    };
    found.unwrap_or((p, p))
}

/// 全屏视图的选区（屏幕坐标）：选中的格子换成选区的样子，文字取自这一帧的行。在视图排好之后调用；同时记下每一屏行的
/// 文字（双击选词用，补到整行宽度，与以前读格子相同）。
pub fn paint_screen(lines: &mut [StyledLine], app: &mut App, width: usize) {
    app.pointer.hits.screen_rows = lines
        .iter()
        .map(|line| {
            let mut text = line.plain();
            let used = string_width(&text);
            if used < width {
                text.push_str(&" ".repeat(width - used));
            }
            text
        })
        .collect();
    let Some(selection) = app.pointer.screen_selection else {
        return;
    };
    let (start, end) =
        if (selection.anchor.1, selection.anchor.0) <= (selection.head.1, selection.head.0) {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
    let last_row = lines.len().saturating_sub(1);
    let (r1, r2) = (
        usize::from(start.1).min(last_row),
        usize::from(end.1).min(last_row),
    );
    let last_col = width.saturating_sub(1);
    for (y, line) in lines.iter_mut().enumerate().take(r2 + 1).skip(r1) {
        let from = if y == r1 { usize::from(start.0) } else { 0 };
        let to = if y == r2 {
            usize::from(end.0).min(last_col)
        } else {
            last_col
        };
        // 行尾之后的格子也是选区：先补到整行。
        pad(line, width);
        crate::ui::selection::mark_line(line, from, to + 1, &app.ink);
    }
    let rows: Vec<Row> = (r1..=r2)
        .map(|y| Row {
            text: app
                .pointer
                .hits
                .screen_rows
                .get(y)
                .cloned()
                .unwrap_or_default(),
            soft: app
                .pointer
                .hits
                .soft
                .iter()
                .find(|(row, _)| usize::from(*row) == y)
                .map(|(_, soft)| *soft),
        })
        .collect();
    app.pointer.hits.selected = Some(join(&rows, usize::from(start.0), usize::from(end.0) + 1));
}

/// 复制结果的提示，盖在 `line`（gap 行或全屏视图的最后一行）的右侧；放不下时截断加 `…`。
pub fn draw_copied(line: &mut StyledLine, app: &App, width: usize) {
    let Some((notice, at)) = &app.pointer.copied else {
        return;
    };
    if at.elapsed() >= notice.duration {
        return;
    }
    /// 提示至少要有这么宽才画。
    const MIN_WIDTH: usize = 2;
    let margin = layout::RIGHT_MARGIN;
    let room = width.saturating_sub(margin);
    if room < MIN_WIDTH {
        return;
    }
    let label = crate::ui::lines::truncate(&notice.text, room);
    let w = string_width(&label);
    let piece = StyledLine::new(vec![Span::new(label, Style::fg(app.ink.accent()))]);
    *line = overlay(line, width.saturating_sub(w + margin), piece);
}

/// 悬停的反馈（tui-select.md §9）：鼠标移到的可点击区域换成更亮一档的底色；16 色终端加下划线。在每一帧记下全部可点击区域之后
/// 调用。
pub fn paint_hover(lines: &mut [StyledLine], app: &App, width: usize) {
    let Some(rect) = app.hovered_region() else {
        return;
    };
    let hover = app.ink.hover_bg();
    let to = usize::from(rect.right()).min(width);
    for row in rect.top()..rect.bottom() {
        let Some(line) = lines.get_mut(usize::from(row)) else {
            break;
        };
        pad(line, to);
        *line = line.restyle_cells(usize::from(rect.x), to, |style| match hover {
            Some(bg) => style.with_bg(bg),
            None => style.with_attr(attr::UNDERLINE),
        });
    }
}
