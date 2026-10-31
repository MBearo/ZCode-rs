//! 会话里的“有新版本”提示（docs/specs/rust-cli/update.md §7；Codex `UpdateAvailableHistoryCell`）：一个圆角框，
//! 框内宽度取内容最宽一行与（终端宽度 − 4）的小者，超出时折行。只显示，不进会话、不落库。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::i18n::Lang;
use super::i18n_update as text;
use super::layout;
use super::palette::Palette;
use super::text::wrap;
use crate::update::UpdateInfo;

/// 提示的行（宽度 `width`）。
pub fn lines(info: &UpdateInfo, palette: &Palette, lang: Lang, width: usize) -> Vec<Line<'static>> {
    let accent = Style::default().fg(palette.accent);
    let bold = Modifier::BOLD;
    let (run, update) = text::notice_run(lang);
    let content = [
        Line::from(vec![
            Span::styled("✨\u{200A}", accent.add_modifier(bold)),
            Span::styled(text::notice_title(lang), accent.add_modifier(bold)),
            Span::raw(" "),
            Span::styled(
                format!("{} -> {}", info.current, info.latest),
                Style::default().add_modifier(bold),
            ),
        ]),
        Line::from(vec![
            Span::raw(run),
            Span::styled(info.command.clone(), accent),
            Span::raw(update),
        ]),
        Line::default(),
        Line::from(text::notice_notes(lang)),
        Line::from(Span::styled(
            info.notes_url.clone(),
            accent.add_modifier(Modifier::UNDERLINED),
        )),
    ];
    let inner = content
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .min(width.saturating_sub(layout::BOX_BORDERS + layout::BOX_PADDING))
        .max(1);
    let border = palette.secondary();
    let mut lines = vec![Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(inner + layout::BOX_PADDING)),
        border,
    ))];
    for line in &content {
        let wrapped = if line.spans.is_empty() {
            vec![Line::default()]
        } else {
            wrap(&line.spans, inner, &[], &[])
        };
        for mut row in wrapped {
            let pad = inner.saturating_sub(row.width());
            row.spans.insert(0, Span::styled("│ ", border));
            row.spans.push(Span::raw(" ".repeat(pad)));
            row.spans.push(Span::styled(" │", border));
            lines.push(row);
        }
    }
    lines.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner + layout::BOX_PADDING)),
        border,
    )));
    lines
}
