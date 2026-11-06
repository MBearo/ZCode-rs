// 改写自 apps/zcode-host/crates/tui/src/ui/draw.rs（feat/rs 79b6691c55）：去掉启动失败页、任务面板、管理
// 页面、回退浏览与加载更早的行；时间由调用方给出。背景标志见 `ui/logo/`。
//! 整屏布局（rust-tui.md §8.1；骨架照 Puffin，docs/specs/rust-cli/tui-kit.md §4.0）：上方 transcript（从顶部开始排，超过
//! 一屏时跟随末尾，可上滚），底部栈贴底。详细视图与独立页面单独占满屏幕。
//!
//! 画法（docs/specs/rust-cli/tui-kit.md §3.2，工作线 TK 的 C2）：排出正好一屏的
//! kit 行（[`Frame`]）交给 runner 的渲染器；叠在上面的东西（悬停、选区）都在排好的行上按列改（`frame.rs`）。光标是那一格的
//! `attr::CARET`。
use std::time::Instant;

use rust_cli_tui_kit::app::frame::{self as skeleton, pinned_row, separator_row};
use rust_cli_tui_kit::render::renderer::Backdrop;
use rust_cli_tui_kit::render::{Color, Style, StyledLine, attr};

use super::app::{App, Overlay};
use super::cells::Ctx;
pub use super::frame::Frame;
use super::frame::caret;
use super::geom::Rect;
use super::ink::Ink;
use super::mouse::{self, Target};
use super::transcript::{self, Cache};

/// 要按动画节拍重画：界面自己的动画（[`App::animating`]），或 transcript 里还有东西在动（播放头、调用与通知的动效，
/// Puffin 的计时器）。runner 与孪生 harness 都按它定节拍；`now` 是动画时钟（同 [`draw`] 的 `now`）。
pub fn animating(app: &App, cache: &Cache, now: Instant) -> bool {
    app.animating(now) || cache.moving()
}

/// 画一帧：`width` × `height` 的终端上正好 `height` 行。`now` 是动画时钟，`now_ms` 是墙上时间（与视图行的时间同一个时钟）。
pub fn draw(
    app: &mut App,
    cache: &mut Cache,
    now: Instant,
    now_ms: u64,
    width: u16,
    height: u16,
) -> Frame {
    // 子代理的用时、倒计时与驱逐按这一帧的墙上时间（agents.rs `AgentList::shown`；按键也用它，下标对得上画出来的行）。
    app.snapshots.agents.shown = now_ms;
    // 复制页的键按这一帧的屏宽重排页（page_copy.rs）。
    app.page_at = (usize::from(width), now_ms);
    // 记下见过在跑的后台子代理（agents_dock.rs）、任务行出现与记号换了的时刻（task_line.rs）；任务卡的种类空了时换或关上
    // （task_card.rs）。
    app.note_dock();
    app.note_line(now);
    app.settle_tasks();
    // 输入层的卡记下这一帧变了的数（cards.rs）。
    app.cards_frame(now, now_ms);
    // 设置页（tui-kit.md §4.2「设置」；Puffin `frame_of` 里它在 transcript 之前）：整屏是它自己的。
    let settings = app.settings_frame(now, now_ms, usize::from(width), usize::from(height));
    let mut lines = if let Some(settings) = settings {
        settings
    } else if let Some(page) = super::page_frame::body(app, usize::from(width), now, now_ms) {
        page_frame(app, &page, now, now_ms, width, height)
    } else if app.overlay.as_ref().is_some_and(Overlay::full_screen) {
        full_screen_frame(app, now_ms, width, height)
    } else {
        main_frame(app, cache, now, now_ms, width, height)
    };
    lines.truncate(usize::from(height));
    lines.resize(usize::from(height), StyledLine::default());
    Frame {
        lines,
        backdrop: backdrop(&app.ink),
    }
}

/// 主题涂的地（tui-kit.md §3.2）：`Theme.paints` 时与 Puffin 相同；离开时还给终端探测到的颜色。
fn backdrop(ink: &Ink) -> Option<Backdrop> {
    let theme = &ink.theme;
    let fg = theme.fg.filter(|_| theme.paints)?;
    let bg = theme.bg?;
    Some(Backdrop {
        fg: fg.color(),
        bg: bg.color(),
        restore: ink
            .foreground
            .zip(ink.background)
            .map(|(f, b)| (f.color(), b.color())),
    })
}

/// 全屏视图：可点击区域与软折行的记号由视图自己记（原来独立页面不重置，鼠标按上一帧 transcript 的记录处理）。
fn full_screen_frame(app: &mut App, now_ms: u64, width: u16, height: u16) -> Vec<StyledLine> {
    app.pointer.hits = mouse::Hits::default();
    let mut lines = full_screen(app, now_ms, width, height);
    lines.resize(usize::from(height), StyledLine::default());
    // 选区画在视图之上，文字取自这一帧的行（F25，tui-select.md §2.1）；复制的提示放在最后一行（提示行）的右侧。
    mouse::paint_screen(&mut lines, app, usize::from(width));
    if let Some(last) = lines.last_mut() {
        mouse::draw_copied(last, app, usize::from(width));
    }
    lines
}

/// 主视图的一屏，照 Puffin 的骨架（docs/specs/rust-cli/tui-kit.md §4.0，Puffin `transcript_frame`，几何在 kit 的
/// `app/frame.rs`）：置顶行（插入）、正文、分隔行（锚定时），然后是底部栈；正文按没有层的一摞量高，开着的层盖在正文最新的
/// 几行上。以前底部区域最高 2/3 屏、上方总留一行 gap，置顶行盖掉正文的第一行，回到底部的标签叠在 gap 行上。
fn main_frame(
    app: &mut App,
    cache: &mut Cache,
    now: Instant,
    now_ms: u64,
    screen_width: u16,
    screen_height: u16,
) -> Vec<StyledLine> {
    app.pointer.screen_selection = None;
    let width = usize::from(screen_width);
    let rows = usize::from(screen_height).max(1);
    let theme = app.ink.theme;
    // 宽度变化后行会重新折行，选区端点不再对应原来的文字：清掉（输入框的选区按字节记，不受影响）。
    let composer_width = super::input_box::text_width(width);
    if app.composer_width != composer_width {
        app.pointer.selection = None;
    }
    app.composer_width = composer_width;
    // transcript 先排：搜索模式（修复线 F114，tui-input.md §2.3）要在画输入框之前定当前匹配与计数；当前匹配换了块时
    // 那一块要按展开的样子再排一次（其余的块都是缓存）。
    let mut layout = transcript_layout(app, cache, width, now, now_ms);
    let mut settled = None;
    if app.port.find.is_some() {
        let before = super::find::peek(app);
        settled = super::find::settle(app, &layout, cache.epoch(), width);
        if super::find::peek(app) != before {
            layout = transcript_layout(app, cache, width, now, now_ms);
        }
    }
    let first = super::pane::bottom(app, width, rows, now, now_ms, None, false);
    let (stack0, asking) = (first.stack0, first.asking);
    let (bottom_count, body_h0) = skeleton::split(rows, stack0, first.status.len());
    let total = layout.len();
    // 跟随的帧（Puffin 的 `end`：不滚、不钉住请求）：顶行是 `total − follow_rows`，置顶行按它的顶行定。锚定的偏移从这个
    // 顶行量（`App::viewport` 是 `follow_rows`），偏移 1 就是它上面一行。
    // 修复：以前偏移从整个正文（`body_h0`）量，跟随的帧有置顶行时离它最近的锚定顶行在上面两行；Puffin 停在上面一行
    // （`scroll_to`：`line < end.top`），最后一格往下滚旧界面已经跟随，Puffin 还停着。
    let prompts = &layout.prompts;
    let max_top0 = total.saturating_sub(body_h0);
    let follow_pin = skeleton::extras(
        body_h0,
        transcript::pinned(prompts, max_top0).is_some(),
        false,
        false,
        asking,
    )
    .0;
    let follow_rows = body_h0.saturating_sub(follow_pin);
    app.set_viewport(follow_rows);
    app.follow_rows = follow_rows;
    app.content_resized(total);
    // 搜索模式：把当前匹配滚进视口、没有匹配时回到原处；esc 之后回到进入前的位置（tui-input.md §2.3.5）。
    super::find::scroll(app, &layout, settled.as_ref(), follow_rows);
    // 拖选到第一行 / 最后一行：每帧滚动 1 行。
    if let Some(selection) = app.pointer.selection
        && selection.dragging
        && selection.edge != 0
    {
        app.scroll_by(selection.edge);
    }
    // 置顶行与分隔行按整个正文的高度决定（kit `frame::extras`），然后为较小的正文再定顶行：锚定（上滚过）时顶行不动，
    // 跟随时停在末尾（Puffin `clamp_top`）。请求开着、还没滚过时（`App::ask_pinned`），它的行与预览比正文高就停在它的行
    // 在顶上（Puffin `ask_line`，tui-kit.md §4.0「请求」）。
    let anchored = app.port.scroll > 0;
    let ask_line = layout.ask_line.filter(|_| asking && app.ask_pinned());
    // 挑选回退的点：视图停在切线上方留 ABOVE 行的位置，到达末尾时照常停在末尾；滚动过就停在滚到的地方（Puffin `at_cut`）。
    let at_cut = layout
        .cut_line
        .filter(|_| app.picking.as_ref().is_some_and(|p| p.at_cut));
    let follow = |h: usize| {
        let max_top = total.saturating_sub(h);
        if let Some(l) = at_cut {
            return l.saturating_sub(super::rewind_pick::ABOVE).min(max_top);
        }
        ask_line
            .filter(|l| total.saturating_sub(*l) > h)
            .map_or(max_top, |l| l.min(max_top))
    };
    let top0 = if anchored {
        total
            .saturating_sub(app.port.scroll)
            .saturating_sub(follow_rows)
    } else {
        follow(body_h0)
    };
    let cutting = layout.cut_line.is_some();
    let (pin_rows, sep_rows) = skeleton::extras(
        body_h0,
        transcript::pinned(prompts, top0).is_some(),
        anchored || cutting,
        top0 < max_top0,
        asking,
    );
    let body_h = body_h0.saturating_sub(pin_rows + sep_rows);
    let max_top = total.saturating_sub(body_h);
    let start = if anchored {
        top0.min(max_top)
    } else {
        follow(body_h)
    };
    let end0 = total.min(start + body_h);
    // 挑选时下面变暗的部分只截在块的边界上（板不会被截成半块），截断处空到正文底（Puffin `vis_end`）。
    let end = layout
        .cut_line
        .filter(|_| sep_rows > 0)
        .and_then(|l| {
            layout
                .blocks
                .iter()
                .map(|b| b.start)
                .rfind(|st| *st > start.max(l) && *st <= end0)
        })
        .unwrap_or(end0);
    // 滚动键从这一帧算（Puffin 从画出的帧与跟随的帧算）。
    app.shown_top = start;
    // 可点击区域与选中的文字每帧重记（选区先记下选中的文字，下面再记底部栈与正文的区域）。
    app.pointer.hits = mouse::Hits::default();
    let range = mouse::select(app, &layout, start, end);
    // 只复制可见行（布局与缓存共享其余的行）。
    let mut visible: Vec<StyledLine> = layout.styled_lines(start, end);
    if let Some(settled) = &settled {
        super::find::highlight(app, &layout, &mut visible, start, settled);
    }
    for (index, line) in visible.iter_mut().enumerate() {
        let l = start + index;
        if let Some((l1, c1, l2, c2)) = range
            && l >= l1
            && l <= l2
        {
            let from = if l == l1 { c1 } else { 0 };
            let to = if l == l2 { c2 } else { usize::MAX };
            super::selection::mark_line(line, from, to, &app.ink);
        }
    }
    // 悬停在可以展开的块的文字上：整个块的 dim 文字变成正文的墨（rich 时 dim 是 `fg3` 的前景色，tui-kit.md §3.8）。
    if let Some(key) = app.hovered_block() {
        for (index, line) in visible.iter_mut().enumerate() {
            if layout.owner(start + index).0 == key {
                for span in &mut line.spans {
                    span.style = undim(span.style, &app.ink);
                }
            }
        }
    }
    let mut upper: Vec<StyledLine> = Vec::with_capacity(body_h0);
    let pinned = (pin_rows > 0)
        .then(|| skeleton::pinned_at(|t| transcript::pinned(prompts, t), top0, start))
        .flatten();
    if pin_rows > 0 {
        let words = pinned.as_ref().map_or("", |(words, _)| words.as_str());
        upper.push(pinned_row(&theme, words, width));
    }
    let shown_rows = visible.len();
    upper.extend(visible);
    upper.resize(pin_rows + body_h, StyledLine::default());
    // 新会话的背景标志（tui.md §7.3）：画在层之下，按没有层的正文量位置，开着的层只是盖住它。
    let mark = app
        .shows_mark()
        .then(|| (0..total).rfind(|l| !layout.styled(*l).plain().trim().is_empty()))
        .flatten()
        .and_then(|last| super::logo::stage(&theme, width, last + 2, body_h0));
    if let Some(stage) = &mark {
        app.mark.paint(&mut upper, stage, now);
    }
    if sep_rows > 0 {
        upper.push(separator_row(&theme, total - end, width, cutting));
    }
    // 请求开着、正文下面还有行时（长的预览）再排一次底部栈：槽是 `PgDn read on`（Puffin `stacks(…, true)`）；槽只占输入行，
    // 两次一样高。
    let super::pane::Bottom {
        shelf,
        status,
        cursor,
        targets,
        composer,
        ..
    } = if asking && end < total {
        super::pane::bottom(app, width, rows, now, now_ms, None, true)
    } else {
        first
    };
    let tail_len = shelf.len() + status.len();
    let skeleton::Spliced {
        lines: mut screen,
        over,
    } = skeleton::splice(upper, stack0, shelf, status, bottom_count);
    // 底部栈接在留下的上半部之后：上半部是 `body_h0 − max(over, 0)` 行，下面是底部栈的最后 `bottom + over` 行。
    let kept_upper = body_h0.saturating_sub(over.max(0) as usize);
    let keep = (bottom_count as i64 + over).max(0) as usize;
    let tail = Tail {
        first: tail_len.saturating_sub(keep),
        top: kept_upper,
    };
    place(app, &mut screen, &tail, cursor, targets, composer, width);
    // 输入框的选区：复制时取这里的文字（transcript 的选区在下面算，两者互斥）。
    if let Some((from, to)) = app.composer.area.selection() {
        app.pointer.hits.selected = Some(app.composer.area.text()[from..to].to_owned());
    }
    // 点击标志按这一帧的位置（`Hits` 每帧重记，页与设置页里没有，动作也就停止重画）。
    app.pointer.hits.mark = mark.map(|mut stage| {
        stage.shown = kept_upper;
        stage
    });
    // 正文里看得见（没有被层盖住）的行：选区、点击展开与双击选词按它们定位。
    let open_rows = shown_rows.min(kept_upper.saturating_sub(pin_rows));
    app.pointer.hits.transcript = Rect::new(0, pin_rows as u16, screen_width, open_rows as u16);
    app.pointer.hits.rows = (start..start + open_rows)
        .map(|l| (layout.owner(l), layout.styled(l).plain()))
        .collect();
    app.pointer.hits.expandable = (start..start + open_rows)
        .filter_map(|l| layout.expandable(l))
        .collect();
    // 置顶行：点击跳到这条消息（旧界面的能力）；分隔行：点击回到底部（同 ctrl+n）。
    if let Some((_, at)) = pinned {
        app.pointer
            .hits
            .regions
            .push((Rect::new(0, 0, screen_width, 1), Target::Jump(at)));
    }
    let sep_y = pin_rows + body_h;
    if sep_rows > 0 && sep_y < kept_upper {
        app.pointer
            .hits
            .regions
            .push((Rect::new(0, sep_y as u16, screen_width, 1), Target::Bottom));
    }
    // 悬停的反馈放在最后：这一帧的可点击区域都已记下。
    mouse::paint_hover(&mut screen, app, width);
    screen
}

/// 底部栈在屏幕上的位置：它的第 `first` 行（之前的被截掉）画在屏幕的第 `top` 行。
struct Tail {
    first: usize,
    top: usize,
}

/// 底部栈的光标、可点击区域与输入框的文字区，从底部栈的行号换成屏幕坐标（主视图与页共用）。
fn place(
    app: &mut App,
    screen: &mut [StyledLine],
    tail: &Tail,
    cursor: Option<(u16, u16)>,
    targets: Vec<super::pane::RowTarget>,
    composer: Option<(usize, usize, usize)>,
    width: usize,
) {
    let tail_y = |row: usize| (row >= tail.first).then(|| (tail.top + row - tail.first) as u16);
    // 光标：那一格的段加 CARET（tui-kit.md §3.2「底部的光标」）。
    if let Some((row, col)) = cursor
        && let Some(y) = tail_y(usize::from(row))
        && let Some(line) = screen.get_mut(usize::from(y))
    {
        caret(line, usize::from(col));
    }
    // 可点击区域：底部栈的行号换成屏幕坐标。
    for (row, col, w, target) in targets {
        if let Some(y) = tail_y(row) {
            let w = w.unwrap_or(width.saturating_sub(col));
            app.pointer
                .hits
                .regions
                .push((Rect::new(col as u16, y, w as u16, 1), target));
        }
    }
    if let Some((row, text_rows, first)) = composer
        && let Some(y) = tail_y(row)
    {
        app.pointer.hits.composer = Some((
            // 输入框的文字从第 3 列起（Puffin 的 ` › `，S2 的 `input_box::TEXT_COL`）。
            Rect::new(
                super::input_box::TEXT_COL as u16,
                y,
                super::input_box::text_width(width) as u16,
                text_rows as u16,
            ),
            first,
        ));
    }
}

/// 一页代替正文的一屏（`page_frame`；Puffin `usage_frame`）：底部栈（没有层与坞，槽是页的键）至多 `行数 − 1` 行，页的
/// 行填满它上面。页里的点击与选区由页的主人记。
fn page_frame(
    app: &mut App,
    page: &super::page_frame::PageBody,
    now: Instant,
    now_ms: u64,
    screen_width: u16,
    screen_height: u16,
) -> Vec<StyledLine> {
    let width = usize::from(screen_width);
    let rows = usize::from(screen_height).max(1);
    app.pointer.clear_selection();
    app.composer_width = super::input_box::text_width(width);
    app.pointer.hits = mouse::Hits::default();
    let super::pane::Bottom {
        shelf,
        status,
        cursor,
        targets,
        composer,
        ..
    } = super::pane::bottom(app, width, rows, now, now_ms, Some(page), false);
    let mut tail = shelf;
    tail.extend(status);
    let total = tail.len();
    let bottom = skeleton::page_bottom(tail, rows);
    // 驾驶舱的行按这一帧底部栈之上的高度排，可点的行记下来。
    let fitted = page.fit.then(|| {
        let height = rows.saturating_sub(bottom.len());
        let (lines, hits) = app.cockpit_screen(width, height, now, now_ms);
        for (row, index) in hits {
            app.pointer.hits.regions.push((
                Rect::new(0, row as u16, screen_width, 1),
                mouse::Target::Cockpit(index),
            ));
        }
        lines
    });
    let body_lines = fitted.as_deref().unwrap_or(&page.lines);
    // 页的键按这一帧的几何滚动（`App::page_rows`）。
    app.page_rows = (rows.saturating_sub(bottom.len()).max(1), body_lines.len());
    let tail = Tail {
        first: total - bottom.len(),
        top: rows - bottom.len(),
    };
    let (mut screen, _) = skeleton::page(&app.ink.theme, body_lines, page.top, bottom, rows);
    place(app, &mut screen, &tail, cursor, targets, composer, width);
    if let Some((from, to)) = app.composer.area.selection() {
        app.pointer.hits.selected = Some(app.composer.area.text()[from..to].to_owned());
    }
    mouse::paint_hover(&mut screen, app, width);
    screen
}

/// 悬停时“去 dim”的一段：`fg3` 的前景色换回正文（默认前景），去掉 `attr::DIM`。plain 主题里 `fg3` 就是 dim 位。
fn undim(style: Style, ink: &Ink) -> Style {
    let dim = ink.theme.fg3.fg_color();
    let fg = if !dim.is_default() && style.fg_color() == dim {
        Color::Default
    } else {
        style.fg_color()
    };
    Style::new(fg, style.bg_color(), style.attrs() & !attr::DIM)
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
        ink: &app.ink,
        lang: app.lang,
        width,
        expanded: app.details_open,
        elapsed: now.duration_since(app.started),
        now_ms,
        workspace,
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        skills: app.composer.mentions.catalog.as_deref().unwrap_or(&[]),
        markdown: None,
    };
    transcript::lines(app, cache, &ctx)
}

/// 全屏视图（`Overlay::full_screen`）的行。
fn full_screen(app: &mut App, _now_ms: u64, width: u16, height: u16) -> Vec<StyledLine> {
    // 全屏视图各出正好一屏的行（tui-kit.md §3.2）；视图的状态在 `app.overlay` 里，画完写回。
    match app.overlay.clone() {
        Some(Overlay::Trust(_)) => {
            // 取出来画，画完放回。
            let overlay = app.overlay.take();
            let mut lines = Vec::new();
            let overlay = match overlay {
                Some(Overlay::Trust(ask)) => {
                    lines = super::trust_ask::lines(app, &ask, width, height);
                    Some(Overlay::Trust(ask))
                }
                other => other,
            };
            app.overlay = overlay;
            lines
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[path = "draw_page_tests.rs"]
mod page_tests;
