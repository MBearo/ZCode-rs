// 改写自 apps/zcode-host/crates/tui/src/ui/page.rs（feat/rs 79b6691c55）：去掉 `/diff` 的查看器与 `/context`。
//! 独立页面（`/status`、`/help`、`/context`、← 会话面板）：占满整个屏幕，第一行是粗体标题，下面是内容，
//! 最后一行是提示。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::{Action, App, Local, Overlay};
use super::cells::Ctx;
use super::footer;
use super::transcript;

/// 独立页面的框架（tui-panels.md §4.3）：第一行粗体标题、空一行、正文、最后一行按键提示（`{键}` 模板，
/// 按键提示组件画，tui.md §7.4）。正文由调用方按可见高度（[`body_height`]）截好；`/status` 等页面与 ← 会话面板共用。
pub fn frame(
    frame: &mut Frame<'_>,
    palette: &super::palette::Palette,
    title: &str,
    body: Vec<Line<'static>>,
    hint: &str,
) {
    let area = frame.area();
    let mut out = vec![
        Line::from(Span::styled(
            format!("  {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
    ];
    out.extend(body.into_iter().take(body_height(area.height)));
    frame.render_widget(Paragraph::new(out), area);
    let mut spans = vec![Span::raw("  ")];
    spans.extend(footer::hint(hint, palette));
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
    );
}

/// 正文以外的行：标题、空行与提示。
const CHROME_ROWS: u16 = 3;

/// 正文可见的行数：去掉标题、空行与提示。
pub fn body_height(height: u16) -> usize {
    height.saturating_sub(CHROME_ROWS) as usize
}

/// 正文的第一行在屏幕上的行号（点击区域用）。
pub const BODY_TOP: u16 = 2;

impl App {
    /// 独立页面的按键：esc、q、ctrl+c 关闭，方向键与翻页滚动。
    pub(super) fn page_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = super::keyboard::ctrl(&key);
        let page = self.viewport.max(1);
        let Some(Overlay::Page { scroll, .. }) = self.overlay.as_mut() else {
            return None;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('c') if ctrl => self.overlay = None,
            _ => {
                if let Some(step) = super::pager::page_move(&key) {
                    super::pager::scroll_from_top(scroll, step, page);
                }
            }
        }
        None
    }
}

/// 画独立页面；`scroll` 是从 `app.overlay` 里取出来的页面的滚动量（夹住后写回，画完由调用方放回）。
pub fn page(frame: &mut Frame<'_>, app: &mut App, local: &Local, scroll: &mut usize, now_ms: u64) {
    let area = frame.area();
    let width = area.width as usize;
    let height = body_height(area.height);
    app.viewport = height;
    let label = crate::ui::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let ctx = Ctx {
        palette: &app.palette,
        lang: app.lang,
        width,
        expanded: true,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: app.view.workspace().root.as_str(),
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        markdown: None,
    };
    let (title, mut lines) = match local {
        Local::Table { title, .. } => {
            // 标题在页面第一行，去掉表格自己的标题行。
            let mut lines = transcript::local_lines(local, &ctx);
            lines.remove(0);
            (title.clone(), lines)
        }
        // `/context`：标题在页面第一行，正文是方格图、明细与建议（tui-panels.md §4.2）。
        Local::Context { .. } => (
            super::context::title(app.lang).to_owned(),
            transcript::local_lines(local, &ctx),
        ),
        Local::Goal(panel) => {
            let mut lines = transcript::local_lines(local, &ctx);
            lines.remove(0);
            (panel.title.clone(), lines)
        }
        _ => (
            app.strings.status_title.to_owned(),
            transcript::local_lines(local, &ctx),
        ),
    };
    // 缺陷修复（F25）：滚动量原来只在这里夹住、不写回，按键与滚轮可以无限累加，往回滚要多滚很多格。
    *scroll = (*scroll).min(lines.len().saturating_sub(height));
    lines.drain(..*scroll);
    // 正文里软折行的续行（屏幕行号），全屏视图的选区据此拼接（tui-select.md §2.3）。
    app.pointer.hits.soft = lines
        .iter()
        .take(height)
        .enumerate()
        .filter_map(|(index, line)| {
            let soft = super::text::soft_wrap(line)?;
            Some((BODY_TOP + u16::try_from(index).ok()?, soft))
        })
        .collect();
    self::frame(frame, &app.palette, &title, lines, app.strings.page_hint);
}
