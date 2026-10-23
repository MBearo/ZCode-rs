// 改写自 apps/zcode-host/crates/tui/src/ui/page.rs（feat/rs 79b6691c55）：去掉 `/diff` 的查看器与 `/context`。
//! 独立页面（`/status`、`/help`、`/context`、← 会话面板）：占满整个屏幕，第一行是粗体标题，下面是内容，
//! 最后一行是提示。
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::{App, Local, Overlay};
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

/// 正文可见的行数：去掉标题、空行与提示。
pub fn body_height(height: u16) -> usize {
    height.saturating_sub(3) as usize
}

/// 正文的第一行在屏幕上的行号（点击区域用）。
pub const BODY_TOP: u16 = 2;

pub fn page(frame: &mut Frame<'_>, app: &mut App, local: &Local, scroll: usize, now_ms: u64) {
    let area = frame.area();
    let width = area.width as usize;
    let height = body_height(area.height);
    app.viewport = height;
    let label = crate::ui::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let ctx = Ctx {
        palette: &app.palette,
        lang: app.lang,
        strings: app.strings,
        width,
        expanded: true,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: app.view.workspace().root.as_str(),
        workspace_label: &label,
        version: &app.version,
        markdown: None,
    };
    let (title, mut lines) = match local {
        Local::Table { title, .. } | Local::Text { title, .. } => {
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
    let scroll = scroll.min(lines.len().saturating_sub(height));
    // 缺陷修复（F25）：滚动量原来只在这里夹住、不写回，按键与滚轮可以无限累加，往回滚要多滚很多格。
    if let Some(Overlay::Page { scroll: stored, .. }) = app.overlay.as_mut() {
        *stored = scroll;
    }
    lines.drain(..scroll);
    // 正文里软折行的续行（屏幕行号），全屏视图的选区据此拼接（tui-select.md §2.3）。
    app.hits.soft = lines
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
