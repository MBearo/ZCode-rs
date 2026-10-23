//! bash 模式的输入行与输出行（docs/specs/rust-cli/tui-input.md §2.1）：输入是 bash 色的 `! ` 加命令；
//! 输出在 `⎿` 下面，stdout 原色、stderr 红色。
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{Ctx, detail_text, dim, red};
use crate::ui::text::wrap_text;

/// 输出最多显示这么多行，其余收起（展开后全部显示）。
const PREVIEW_LINES: usize = 10;

pub fn input(command: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let block = ctx.palette.prompt_block();
    let first = vec![Span::styled("! ", Style::default().fg(ctx.palette.bash()))];
    let rest = vec![Span::raw("  ")];
    wrap_text(
        command,
        Style::default(),
        ctx.width.saturating_sub(1),
        &first,
        &rest,
    )
    .into_iter()
    .map(|line| line.style(block))
    .collect()
}

pub fn output(stdout: &str, stderr: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let (stdout, stderr) = (stdout.trim_end(), stderr.trim_end());
    if stdout.is_empty() && stderr.is_empty() {
        return detail_text("(No output)", dim(), ctx);
    }
    let mut lines = Vec::new();
    if !stdout.is_empty() {
        lines.extend(detail_text(stdout, Style::default(), ctx));
    }
    if !stderr.is_empty() {
        lines.extend(detail_text(stderr, red(), ctx));
    }
    if !ctx.expanded && lines.len() > PREVIEW_LINES {
        let hidden = lines.len() - PREVIEW_LINES;
        lines.truncate(PREVIEW_LINES);
        lines.push(super::more_lines(hidden, ctx));
    }
    lines
}
