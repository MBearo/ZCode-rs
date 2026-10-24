//! 扣住的同伴消息的批准框（docs/specs/rust-cli/cross-session.md §2.6）：警告色的标题，
//! 发送方、原因、正文预览（最多 8 行、每行 72 列），两个选项（缺省焦点在拒绝），最后一行按键提示。内容只来自
//! 会话视图的 `peer_held`；这里只排版。
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::peers::HeldPeerMessage;

use super::i18n_peers as words;
use super::interaction_view::{View, bold, dashed, dim, hint, named, option};
use super::text::{truncate_line, wrap_text};

/// 预览最多几行、每行多少列。
const PREVIEW_LINES: usize = 8;
const PREVIEW_COLUMNS: usize = 72;

/// 行与选项所在的行：（行号，选项下标）。`choice` 是焦点（0 拒绝，1 投递）。
pub fn lines(
    held: &HeldPeerMessage,
    count: usize,
    choice: usize,
    view: &View<'_>,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let warning = Style::default().fg(Color::Yellow);
    let mut lines = vec![
        Line::from(Span::styled("─".repeat(view.width), warning)),
        truncate_line(
            Line::from(Span::styled(
                format!("  {}", words::title(view.lang)),
                bold().fg(Color::Yellow),
            )),
            view.width,
        ),
    ];
    let from = words::from(view.lang, held.from.as_str(), held.from_name.as_deref());
    lines.extend(wrap_text(
        &from,
        Style::default(),
        view.width,
        &[Span::raw("  ")],
        &[Span::raw("  ")],
    ));
    lines.extend(wrap_text(
        words::cause(view.lang, held.cause),
        dim(),
        view.width,
        &[Span::raw("  ")],
        &[Span::raw("  ")],
    ));
    if count > 1 {
        lines.push(Line::from(Span::styled(
            format!("  {}", words::more(view.lang, count)),
            dim(),
        )));
    }
    if !held.body.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", words::body(view.lang)),
            dim(),
        )));
        lines.push(dashed(view.width));
        let body: Vec<&str> = held.body.lines().collect();
        for line in body.iter().take(PREVIEW_LINES) {
            let clipped: String = line.chars().take(PREVIEW_COLUMNS).collect();
            lines.push(truncate_line(
                Line::from(format!("  {clipped}")),
                view.width,
            ));
        }
        if body.len() > PREVIEW_LINES {
            lines.push(Line::from(Span::styled(
                format!("  … {}", body.len() - PREVIEW_LINES),
                dim(),
            )));
        }
        lines.push(dashed(view.width));
    }
    let mut options = Vec::new();
    for (index, label) in words::options(view.lang).into_iter().enumerate() {
        options.push((lines.len(), index));
        let focused = index == choice;
        lines.push(option(
            focused,
            index + 1,
            vec![named(label, focused, view)],
            view,
        ));
    }
    lines.push(hint(words::hint(view.lang), view));
    (lines, options)
}
