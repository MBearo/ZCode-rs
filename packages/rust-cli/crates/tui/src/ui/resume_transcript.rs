//! `/resume` 页的全屏只读对话（修复线 F29，docs/specs/rust-cli/tui-manage.md §10.3）：
//! 会话的 transcript 用主界面同一套块的排版（`cells::build` + `cells::render`，普通形式），独立页面的框架，可滚动。
use ratatui::Frame;
use ratatui::text::Line;

use super::app::App;
use super::cells::{self, Ctx};
use super::i18n_projects as words;
use super::resume::Shown;

/// 会话的全部行（块之间空一行，同主界面）。
pub fn lines(app: &App, shown: &Shown, width: usize, now_ms: u64) -> Vec<Line<'static>> {
    let workspace = shown.view.workspace().root.as_str().to_owned();
    let label = super::app::label(&workspace, app.home.as_deref());
    let ctx = Ctx {
        palette: &app.palette,
        lang: app.lang,
        strings: app.strings,
        width,
        expanded: false,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: &workspace,
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        markdown: None,
    };
    let mut out: Vec<Line<'static>> = Vec::new();
    for cell in cells::build(shown.view.rows(), true, &app.mcp_machines) {
        let rendered = cells::render(&cell, &ctx);
        if rendered.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(rendered);
    }
    out
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App, shown: &mut Shown, now_ms: u64) {
    let area = frame.area();
    let height = super::page::body_height(area.height);
    app.viewport = height;
    let mut body = lines(app, shown, area.width as usize, now_ms);
    shown.scroll = shown.scroll.min(body.len().saturating_sub(height));
    body.drain(..shown.scroll);
    super::page::frame(
        frame,
        &app.palette,
        &shown.title,
        body,
        words::transcript_hint(app.lang),
    );
}
