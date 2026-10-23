// 复制自 apps/zcode-host/crates/tui/src/ui/markdown_table.rs（feat/rs 79b6691c55）。
//! markdown 表格（docs/specs/rust-tui.md §8.2）：放得下时画线框、表头粗体，表头下一条分隔线；放不下
//! 时每行转成 `表头: 值` 的记录。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::{Renderer, styled};
use crate::ui::text::{width, wrap};

impl Renderer<'_> {
    pub(super) fn table(&mut self, rows: Vec<Vec<String>>) {
        self.blank();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0usize; columns];
        for row in &rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.width());
            }
        }
        let (_, rest) = self.prefixes_peek();
        let room = self.width.saturating_sub(width(&rest));
        // 线框：每格左右各 1 个空格，列之间与两侧各 1 条竖线。
        let total: usize = widths.iter().sum::<usize>() + 3 * columns + 1;
        if total > room {
            // 放不下：每行转成 `表头: 值` 的记录。
            let header = rows.first().cloned().unwrap_or_default();
            for row in rows.iter().skip(1) {
                for (index, cell) in row.iter().enumerate() {
                    let key = header.get(index).cloned().unwrap_or_default();
                    let spans = [
                        styled(
                            &format!("{key}: "),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(cell.clone()),
                    ];
                    let (first, rest) = self.prefixes();
                    self.lines.extend(wrap(&spans, self.width, &first, &rest));
                }
                let (_, rest) = self.prefixes_peek();
                self.lines.push(Line::from(rest));
            }
            self.gap = true;
            return;
        }
        let dim = Style::default().add_modifier(Modifier::DIM);
        let rule = |left: &str, middle: &str, right: &str| {
            let cells: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            styled(&format!("{left}{}{right}", cells.join(middle)), dim)
        };
        let push = |this: &mut Self, spans: Vec<Span<'static>>| {
            let (first, _) = this.prefixes();
            let mut line = first;
            line.extend(spans);
            this.lines.push(Line::from(line));
        };
        push(self, vec![rule("┌", "┬", "┐")]);
        for (row_index, row) in rows.iter().enumerate() {
            let style = if row_index == 0 {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut spans = vec![styled("│", dim)];
            for (index, width) in widths.iter().enumerate() {
                let cell = row.get(index).cloned().unwrap_or_default();
                let pad = width.saturating_sub(cell.width());
                spans.push(styled(&format!(" {cell}{} ", " ".repeat(pad)), style));
                spans.push(styled("│", dim));
            }
            push(self, spans);
            if row_index == 0 && rows.len() > 1 {
                push(self, vec![rule("├", "┼", "┤")]);
            }
        }
        push(self, vec![rule("└", "┴", "┘")]);
        self.gap = true;
    }
}
