//! 带预览的题的排版（docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）：左边 30 字宽的选项列（`❯` 是焦点，选中的
//! 是成功色、带 ` ✔`），右边焦点所在选项的预览框（`┌─┐│└─┘`，按 markdown 画），框下空一行是备注。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{View, bold, dim, input};
use crate::ui::i18n::Lang;
use crate::ui::interaction::{Interaction, Question};
use crate::ui::layout;
use crate::ui::markdown;
use crate::ui::text::{truncate_line, width};

/// 选项列的宽度。
const COLUMN: usize = 30;
/// 预览框的最小内宽。
const MIN_INNER: usize = 40;
/// 屏宽减它是预览框的最大宽度（选项列与间隔）。
const RESERVED: usize = 34;
/// 屏高减它是预览框里最多的行数。
const RESERVED_ROWS: usize = 26;

/// 预览框的最大内宽：屏宽减选项列与间隔，再减框线与两侧的空格。
fn max_inner(view: &View<'_>) -> usize {
    view.width
        .saturating_sub(RESERVED + layout::BOX_BORDERS + layout::BOX_PADDING)
        .max(1)
}

fn rendered(preview: &str, view: &View<'_>) -> Vec<Line<'static>> {
    markdown::render(
        preview,
        max_inner(view),
        Vec::new(),
        Vec::new(),
        Style::default(),
        view.palette,
    )
}

/// 预览框的行；`inner` 是框的内宽。
fn boxed(preview: Option<&str>, inner: usize, view: &View<'_>) -> Vec<Line<'static>> {
    let mut body = match preview {
        Some(preview) => rendered(preview, view),
        None => vec![Line::from(match view.lang {
            Lang::En => "No preview available",
            Lang::Zh => "没有预览",
        })],
    };
    let limit = view.height.saturating_sub(RESERVED_ROWS).max(1);
    let hidden = body.len().saturating_sub(limit);
    body.truncate(limit);
    let mut out = vec![Line::from(format!(
        "┌{}┐",
        "─".repeat(inner + layout::BOX_PADDING)
    ))];
    for line in body {
        let line = truncate_line(line, inner);
        let pad = inner.saturating_sub(width(&line.spans));
        let mut spans = vec![Span::raw("│ ")];
        spans.extend(line.spans);
        spans.push(Span::raw(format!("{} │", " ".repeat(pad))));
        out.push(Line::from(spans));
    }
    if hidden > 0 {
        let label = format!("─── ✂ ─── {hidden} lines hidden ");
        let fill = (inner + layout::BOX_PADDING).saturating_sub(label.chars().count());
        out.push(Line::from(format!("├{label}{}┤", "─".repeat(fill))));
    }
    out.push(Line::from(format!(
        "└{}┘",
        "─".repeat(inner + layout::BOX_PADDING)
    )));
    out
}

impl Interaction {
    /// 只有一题时的标签行：`☐ 题头`（选过时 `☒`）。
    pub(super) fn single_tab(&self, question: &Question) -> Line<'static> {
        let mark = if self.drafts[0].values.is_empty() {
            "☐"
        } else {
            "☒"
        };
        Line::from(format!("  {mark} {}", question.header))
    }

    /// 问题与选项列、预览框、备注；返回选项所在的行。
    pub(super) fn preview_body(
        &self,
        question: &Question,
        lines: &mut Vec<Line<'static>>,
        view: &View<'_>,
    ) -> Vec<(usize, usize)> {
        let draft = &self.drafts[self.question];
        if !question.text.trim().is_empty() {
            lines.push(truncate_line(
                Line::from(Span::styled(format!("  {}", question.text.trim()), bold())),
                view.width,
            ));
            lines.push(Line::default());
        }
        // 框的内宽按全部选项的预览定（换焦点时框不变宽窄）。
        let inner = question
            .previews
            .iter()
            .flatten()
            .flat_map(|preview| rendered(preview, view))
            .map(|line| width(&line.spans))
            .fold(MIN_INNER, usize::max)
            .min(max_inner(view));
        let focused = question
            .previews
            .get(self.cursor)
            .and_then(Option::as_deref);
        let accent = Style::default().fg(view.palette.accent);
        let mut right = boxed(focused, inner, view);
        right.push(Line::default());
        let zh = view.lang == Lang::Zh;
        let mut notes = vec![
            Span::styled(if zh { "备注：" } else { "Notes:" }, accent),
            Span::raw(if zh { "" } else { " " }),
        ];
        let quiet = dim().add_modifier(Modifier::ITALIC);
        if self.typing {
            notes.extend(input(
                &draft.notes,
                if zh {
                    "写下对这个方案的备注…"
                } else {
                    "Add notes on this design\u{2026}"
                },
            ));
        } else if draft.notes.is_empty() {
            let hint = if zh {
                "按 n 写备注"
            } else {
                "press n to add notes"
            };
            notes.push(Span::styled(hint, quiet));
        } else {
            notes.push(Span::styled(draft.notes.clone(), quiet));
        }
        right.push(Line::from(notes));
        // 选中的选项与对勾用成功色（与普通题同一个色，tui.md §7.6）。
        let green = view.palette.success();
        let mut options = Vec::new();
        let start = lines.len();
        for row in 0..right.len().max(question.options.len()) {
            let mut spans = vec![Span::raw("  ")];
            if let Some((label, _)) = question.options.get(row) {
                let chosen = draft.values.first() == Some(label);
                let here = row == self.cursor;
                let mut style = match (chosen, here) {
                    (true, _) => green,
                    (false, true) => accent,
                    (false, false) => Style::default(),
                };
                if here {
                    style = style.add_modifier(Modifier::BOLD);
                }
                let mut left = vec![
                    if here {
                        Span::styled("❯", accent)
                    } else {
                        Span::raw(" ")
                    },
                    Span::styled(format!(" {}.", row + 1), dim()),
                    Span::styled(format!(" {label}"), style),
                ];
                if chosen {
                    left.push(Span::styled(" ✔", green));
                }
                let left = truncate_line(Line::from(left), COLUMN);
                let pad = COLUMN.saturating_sub(width(&left.spans));
                spans.extend(left.spans);
                spans.push(Span::raw(" ".repeat(pad)));
                options.push((start + row, row));
            } else {
                spans.push(Span::raw(" ".repeat(COLUMN)));
            }
            spans.push(Span::raw("  "));
            if let Some(line) = right.get(row) {
                spans.extend(line.spans.clone());
            }
            lines.push(truncate_line(Line::from(spans), view.width));
        }
        options
    }
}
