// 改写自 apps/zcode-host/crates/tui/src/ui/draw.rs（feat/rs 79b6691c55）：去掉启动失败页、任务面板、管理
// 页面、回退浏览与加载更早的行；时间由调用方给出。背景标志按 2f982570bb 接回（修复线 F26）。
//! 整屏布局（rust-tui.md §8.1）：上方 transcript（从顶部开始排，超过一屏时跟随末尾，可上滚），中间 1 行
//! gap，底部区域贴底。详细视图与独立页面单独占满屏幕。
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::app::{App, Overlay};
use super::cells::Ctx;
use super::mouse::{self, Target};
use super::transcript::{self, Cache};

/// 底部区域最高占屏幕的 2/3，至少 8 行。
const BOTTOM_SHARE: (usize, usize) = (2, 3);
const MIN_BOTTOM_ROWS: usize = 8;
/// 已滚出顶部的提问固定在第一行：区域至少这么高、这么宽时才固定。
const PIN_MIN_HEIGHT: usize = 4;
const PIN_MIN_WIDTH: usize = 16;

/// 带底色的行补到整行宽度（底色铺满整行）。
pub(super) fn fill_rows(lines: &mut [Line<'static>], width: usize) {
    for line in lines {
        if line.style.bg.is_some() {
            let used = line.width();
            if used < width {
                line.spans
                    .push(Span::styled(" ".repeat(width - used), line.style));
            }
        }
    }
}

/// 画一帧。`now` 是动画时钟，`now_ms` 是墙上时间（与视图行的时间同一个时钟）。
pub fn draw(frame: &mut Frame<'_>, app: &mut App, cache: &mut Cache, now: Instant, now_ms: u64) {
    if app.overlay.as_ref().is_some_and(mouse::full_screen) {
        // 全屏视图：可点击区域与软折行的记号由视图自己记（原来独立页面不重置，鼠标按上一帧 transcript 的记录处理）。
        app.hits = mouse::Hits::default();
        full_screen(frame, app, cache, now, now_ms);
        // 选区画在视图之上，文字取自这一帧的屏幕（F25，tui-select.md §2.1）；复制的提示放在最后一行（提示行）的右侧。
        mouse::paint_screen(frame, app);
        let area = frame.area();
        let last = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        mouse::draw_copied(frame, app, last);
        return;
    }
    app.screen_selection = None;
    let area = frame.area();
    let width = area.width as usize;
    // 宽度变化后行会重新折行，选区端点不再对应原来的文字：清掉（输入框的选区按字节记，不受影响）。
    let composer_width = super::input_box::text_width(width);
    if app.composer_width != composer_width {
        app.selection = None;
    }
    app.composer_width = composer_width;
    // transcript 先排：搜索模式（修复线 F114，tui-input.md §2.3）要在画输入框之前定当前匹配与计数；当前匹配换了块时
    // 那一块要按展开的样子再排一次（其余的块都是缓存）。
    let mut layout = transcript_layout(app, cache, width, now, now_ms);
    let mut settled = None;
    if app.find.is_some() {
        let before = super::find::peek(app);
        settled = super::find::settle(app, &layout, cache.epoch(), width);
        if super::find::peek(app) != before {
            layout = transcript_layout(app, cache, width, now, now_ms);
        }
    }
    // 底部区域贴底，最高 max(8, 2/3 屏)；上方留 1 行 gap 给 transcript 与它隔开。
    let max_bottom = (area.height as usize * BOTTOM_SHARE.0 / BOTTOM_SHARE.1)
        .max(MIN_BOTTOM_ROWS)
        .min((area.height as usize).saturating_sub(1));
    let super::pane::Bottom {
        lines: mut bottom,
        mut cursor,
        targets,
        composer,
    } = super::pane::bottom_lines(app, width, area.height as usize, max_bottom, now, now_ms);
    // 底部太高时从上方截掉（状态行与排队预览先让位）。
    let overflow = bottom.len().saturating_sub(max_bottom);
    if overflow > 0 {
        bottom.drain(..overflow);
        cursor = cursor.map(|(row, col)| (row.saturating_sub(overflow as u16), col));
    }
    fill_rows(&mut bottom, width);
    let bottom_height = bottom.len() as u16;
    let bottom_area = Rect::new(
        area.x,
        area.bottom() - bottom_height,
        area.width,
        bottom_height,
    );
    frame.render_widget(Paragraph::new(bottom), bottom_area);
    if let Some((row, col)) = cursor {
        frame.set_cursor_position((bottom_area.x + col, bottom_area.y + row));
    }
    // 可点击区域：每帧重记。
    app.hits = mouse::Hits::default();
    let row_y = |row: usize| (row >= overflow).then(|| bottom_area.y + (row - overflow) as u16);
    for (row, col, w, target) in targets {
        if let Some(y) = row_y(row) {
            let w = w.unwrap_or(width.saturating_sub(col));
            app.hits
                .regions
                .push((Rect::new(area.x + col as u16, y, w as u16, 1), target));
        }
    }
    if let Some((row, rows, first)) = composer
        && let Some(y) = row_y(row)
    {
        app.hits.composer = Some((
            Rect::new(
                area.x + super::input_box::PROMPT_WIDTH as u16,
                y,
                composer_width as u16,
                rows as u16,
            ),
            first,
        ));
    }
    // 输入框的选区：复制时取这里的文字（transcript 的选区在下面算，两者互斥）。
    if let Some((start, end)) = app.composer.area.selection() {
        app.hits.selected = Some(app.composer.area.text()[start..end].to_owned());
    }
    let gap = Rect::new(
        area.x,
        bottom_area.y.saturating_sub(1),
        area.width,
        u16::from(bottom_area.y > area.y),
    );
    let transcript_area = Rect::new(area.x, area.y, area.width, gap.y - area.y);
    let total = layout.len();
    let height = transcript_area.height as usize;
    app.viewport = height;
    app.content_resized(total);
    // 搜索模式：把当前匹配滚进视口、没有匹配时回到原处；esc 之后回到进入前的位置（tui-input.md §2.3.5）。
    super::find::scroll(app, &layout, settled.as_ref(), height);
    // 拖选到第一行 / 最后一行：每帧滚动 1 行。
    if let Some(selection) = app.selection
        && selection.dragging
        && selection.edge != 0
    {
        app.scroll_by(selection.edge);
    }
    // 从顶部开始排：不满一屏时会话头在第一行；超过一屏时跟随末尾，上滚时按 scroll 偏移。
    let end = total.saturating_sub(app.scroll);
    let start = end.saturating_sub(height);
    let range = mouse::select(app, &layout, start, end);
    // 只复制可见行（布局与缓存共享其余的行）。
    let mut visible: Vec<Line<'static>> = layout.lines(start, end);
    if let Some(settled) = &settled {
        super::find::paint(app, &layout, &mut visible, start, settled);
    }
    for (index, line) in visible.iter_mut().enumerate() {
        let l = start + index;
        if let Some((l1, c1, l2, c2)) = range
            && l >= l1
            && l <= l2
        {
            let from = if l == l1 { c1 } else { 0 };
            let to = if l == l2 { c2 } else { usize::MAX };
            super::selection::highlight(line, from, to, app.palette.selection_bg);
        }
    }
    app.hits.transcript = transcript_area;
    app.hits.rows = (start..end)
        .map(|l| (layout.owner(l), super::text::plain(layout.line(l))))
        .collect();
    app.hits.expandable = (start..end).filter_map(|l| layout.expandable(l)).collect();
    // 提问已滚出顶部时固定在第一行（区域太小时不固定）；点击跳到这条消息。
    let prompts = &layout.prompts;
    let mut pinned = false;
    if height >= PIN_MIN_HEIGHT
        && width >= PIN_MIN_WIDTH
        && let Some(pin) = transcript::pinned(prompts, start, width, app.palette.prompt_block())
        && let Some(first) = visible.first_mut()
    {
        *first = pin;
        pinned = true;
        if let Some(prompt) = prompts.iter().rev().find(|p| p.start < start) {
            app.hits.regions.push((
                Rect::new(area.x, transcript_area.y, area.width, 1),
                Target::Jump(prompt.start),
            ));
        }
    }
    // 悬停在可以展开的块的文字上：整个块的 dim 文字变成正常亮度。
    if let Some(key) = app.hovered_block() {
        for (index, line) in visible.iter_mut().enumerate() {
            if !(pinned && index == 0) && layout.owner(start + index).0 == key {
                line.style = line.style.remove_modifier(Modifier::DIM);
                for span in &mut line.spans {
                    span.style = span.style.remove_modifier(Modifier::DIM);
                }
            }
        }
    }
    fill_rows(&mut visible, width);
    let shown = visible.len() as u16;
    frame.render_widget(Paragraph::new(visible), transcript_area);
    // 新会话的背景标志（tui.md §7.3，修复线 F26，复制自 Host TUI）：启动头下面的空白处；有会话、输入框有字或有
    // 视图时不画。
    if app.shows_logo() {
        let free = Rect::new(
            transcript_area.x,
            transcript_area.y + shown + 1,
            transcript_area.width,
            transcript_area.height.saturating_sub(shown + 1),
        );
        app.logo.draw(frame.buffer_mut(), free, &app.palette, now);
        if let Some(stage) = app.logo.stage {
            app.hits.regions.push((stage, Target::Logo));
        }
    } else {
        app.logo.hide();
    }
    mouse::draw_copied(frame, app, gap);
    if app.scroll > 0 && gap.height > 0 {
        let label = super::i18n::follow_labels(app.lang, app.new_activity)
            .into_iter()
            .find(|label| label.width() <= width);
        if let Some(label) = label {
            let label_width = label.width() as u16;
            let target = Rect::new(gap.x + (gap.width - label_width) / 2, gap.y, label_width, 1);
            let style = app.palette.user_block().fg(app.palette.accent);
            frame.render_widget(Paragraph::new(Span::styled(label, style)), target);
            app.hits.regions.push((target, Target::Bottom));
        }
    }
    // 悬停的反馈放在最后：这一帧的可点击区域都已记下。
    mouse::paint_hover(frame, app);
}

/// 普通视图的 transcript 排版。相对路径按现在看的会话的工作区（查看子代理时是子会话的，修复线 F112，tui-panels.md §1.7）。
fn transcript_layout(
    app: &App,
    cache: &mut Cache,
    width: usize,
    now: Instant,
    now_ms: u64,
) -> transcript::Layout {
    let workspace = app.shown().workspace().root.as_str();
    let label = crate::ui::app::label(workspace, app.home.as_deref());
    let ctx = Ctx {
        palette: &app.palette,
        lang: app.lang,
        strings: app.strings,
        width,
        expanded: false,
        elapsed: now.duration_since(app.started),
        now_ms,
        workspace,
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        markdown: None,
    };
    transcript::lines(app, cache, &ctx)
}

/// 全屏视图（`mouse::full_screen`）的绘制。
fn full_screen(frame: &mut Frame<'_>, app: &mut App, cache: &mut Cache, now: Instant, now_ms: u64) {
    match app.overlay.clone() {
        Some(Overlay::Detail(_)) => {
            super::detail::draw(frame, app, cache, now.duration_since(app.started), now_ms);
        }
        Some(Overlay::Page { local, scroll }) => {
            super::page::page(frame, app, &local, scroll, now_ms)
        }
        Some(Overlay::Usage(view)) => super::usage::draw(frame, app, &view),
        Some(Overlay::Projects(_) | Overlay::Resume(_) | Overlay::Trust(_)) => {
            // 取出来画（不每帧复制会话列表与全屏对话），画完放回。
            let overlay = app.overlay.take();
            match &overlay {
                Some(Overlay::Projects(_)) => {
                    let Some(Overlay::Projects(mut page)) = overlay else {
                        return;
                    };
                    super::projects_draw::draw(frame, app, &mut page, now_ms);
                    app.overlay = Some(Overlay::Projects(page));
                    return;
                }
                Some(Overlay::Resume(_)) => {
                    let Some(Overlay::Resume(mut page)) = overlay else {
                        return;
                    };
                    super::resume_draw::draw(frame, app, &mut page, now_ms);
                    app.overlay = Some(Overlay::Resume(page));
                    return;
                }
                Some(Overlay::Trust(ask)) => super::trust_ask::draw(frame, app, ask),
                _ => {}
            }
            app.overlay = overlay;
        }
        _ => {}
    }
}
