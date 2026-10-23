// 改写自 apps/zcode-host/crates/tui/src/ui/pane.rs（feat/rs 79b6691c55）：待处理交互来自视图；去掉登录、
// 后台任务、任务清单与子代理列表。
//! 底部区域的组装：视图替换整个底部区域；否则依次是状态行、排队预览、快捷键面板（F26 起在输入框上面）、
//! 输入框、弹出列表或 footer。同时记下可点击的行。
use std::time::Instant;

use ratatui::text::Line;

use super::app::{App, Overlay};
use super::bottom;
use super::footer;
use super::interaction::View;
use super::mouse::Target;
use super::status_line::{self, Clock};

/// 底部区域的一行可点击区域：（行，起始列，宽度，目标）；宽度为 None 时占整行。
pub(super) type RowTarget = (usize, usize, Option<usize>, Target);

/// 视图的行、其中可点击的行与终端光标（行，列）。
type ViewLines = (Vec<Line<'static>>, Vec<RowTarget>, Option<(u16, u16)>);

/// 替换输入区的视图（列表、审批与提问）；没有视图时返回 None。光标只有提问的“其他”行聚焦时有
/// （tui.md §7.6），别的视图隐藏光标。
fn view_lines(app: &App, width: usize, height: usize, now_ms: u64) -> Option<ViewLines> {
    if let Some(Overlay::Login(view)) = &app.overlay {
        let lines = super::login::lines(view, app.strings, &app.palette, width);
        return Some((lines, Vec::new(), None));
    }
    if let Some(Overlay::Diff(view)) = &app.overlay {
        let lines = super::diff_view_lines::lines(app, view, width, height, now_ms);
        return Some((lines, Vec::new(), None));
    }
    if let Some(Overlay::Export(view)) = &app.overlay {
        let lines = super::export::lines(view, app.lang, &app.palette, width);
        return Some((lines, Vec::new(), None));
    }
    if let Some(Overlay::Permissions(view)) = &app.overlay {
        let lines = super::permissions_lines::lines(view, app.lang, &app.palette, width, height);
        return Some((lines, Vec::new(), None));
    }
    if let Some(Overlay::Background(_)) = &app.overlay {
        let (lines, rows) = app.background_lines(width, height, Instant::now());
        let targets = rows
            .into_iter()
            .map(|(row, index)| (row, 0, None, Target::Background(index)))
            .collect();
        return Some((lines, targets, None));
    }
    if let Some(Overlay::List(list)) = &app.overlay {
        let (lines, items) = list.lines(width, height, app.lang, &app.palette);
        let targets = items
            .into_iter()
            .map(|(row, index)| (row, 0, None, Target::Item(index)))
            .collect();
        return Some((lines, targets, None));
    }
    // 扣住的同伴消息（cross-session.md §2.6）：没有待回答的交互时显示批准框，替换输入区。
    if app.view.pending().is_none()
        && let Some(held) = app.view.peer_held()
    {
        let view = View {
            palette: &app.palette,
            strings: app.strings,
            lang: app.lang,
            width,
            now_ms,
        };
        let (lines, _) =
            super::peer_view::lines(held, app.view.peer_held_count(), app.peer_choice, &view);
        return Some((lines, Vec::new(), None));
    }
    let (_, request) = app.view.pending()?;
    let interaction = app.interaction.as_ref()?;
    let view = View {
        palette: &app.palette,
        strings: app.strings,
        lang: app.lang,
        width,
        now_ms,
    };
    let (lines, options) = interaction.lines(request, &view);
    let targets = options
        .into_iter()
        .map(|(row, index)| (row, 0, None, Target::Choice(index)))
        .collect();
    let cursor = interaction
        .cursor(request, &view)
        .and_then(|(row, column)| Some((u16::try_from(row).ok()?, u16::try_from(column).ok()?)));
    Some((lines, targets, cursor))
}

/// 底部区域：行、光标（相对行，列）、可点击的行、输入框文字区（起始行，行数，第一可见行）。
pub(super) struct Bottom {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) cursor: Option<(u16, u16)>,
    pub(super) targets: Vec<RowTarget>,
    pub(super) composer: Option<(usize, usize, usize)>,
}

pub(super) fn bottom_lines(
    app: &mut App,
    width: usize,
    height: usize,
    max_bottom: usize,
    now: Instant,
    now_ms: u64,
) -> Bottom {
    if let Some((lines, targets, cursor)) = view_lines(app, width, max_bottom, now_ms) {
        return Bottom {
            lines,
            cursor,
            targets,
            composer: None,
        };
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut targets = Vec::new();
    let clock = Clock {
        now_ms,
        animation: now.duration_since(app.started),
        since_frame: now.saturating_duration_since(app.last_frame),
    };
    // 运行中：状态行，前面空 1 行。
    let status = status_line::status(app, width, &clock);
    if !status.is_empty() {
        lines.extend(status);
        lines.push(Line::default());
    }
    // 任务清单（ctrl+t 展开，tui-display.md §2.2）在排队预览之上。
    lines.extend(super::tasks::lines(app, width, height, now));
    // 排队的消息紧贴在输入框上方。
    lines.extend(bottom::queue(app, width));
    // `?` 快捷键面板在输入框上面（tui.md §7.4，修复线 F26，Codex 的位置；原来替换输入框下面的 footer）。
    if matches!(app.overlay, Some(Overlay::Shortcuts)) {
        lines.extend(footer::help(app, width));
    }
    // 输入框（上下两条边线）；文字区最多 max(3, 高度 / 2 − 5) 行，也不超过底部区域剩下的高度。
    let max_rows = super::input_box::max_rows(height)
        .min(max_bottom.saturating_sub(lines.len() + 3))
        .max(1);
    let (input, position, first) = super::input_box::input_box(app, width, max_rows);
    let top = lines.len();
    let text_rows = input.len().saturating_sub(2);
    lines.extend(input);
    let mut cursor = position.map(|(row, col)| (top as u16 + row, col));
    // 输入框下面：弹出列表或 footer（两者替换同一个位置；快捷键面板打开时 footer 写关闭的提示）。
    if app.composer.popup.is_some() {
        let (popup, first) = bottom::popup(app, width, height);
        if let Some(first) = first {
            // 补齐用的空行不可点击。
            for index in (0..popup.len()).filter(|i| popup[*i].width() > 0) {
                targets.push((lines.len() + index, 0, None, Target::Popup(first + index)));
            }
        }
        lines.extend(popup);
    } else {
        let (footer, hints) = footer::footer(app, width);
        let footer_row = lines.len();
        lines.push(footer);
        targets.extend(
            hints
                .into_iter()
                .map(|(col, w, target)| (footer_row, col, Some(w), target)),
        );
        if let Some(col) = footer::search_cursor(app) {
            cursor = Some(((lines.len() - 1) as u16, col));
        }
        // footer 下面的子代理列表（tui-panels.md §1.2）。
        let (agents, rows) = app.agents_lines(width, now);
        let first = lines.len();
        targets.extend(
            rows.into_iter()
                .map(|(row, index)| (first + row, 0, None, Target::Agent(index))),
        );
        lines.extend(agents);
    }
    Bottom {
        lines,
        cursor,
        targets,
        composer: Some((top + 1, text_rows, first)),
    }
}
