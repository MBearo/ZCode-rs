//! 按铺的宽度铺表：各列放得下时按内容宽，否则按 [`column_widths`] 分享宽度、格子按词折行。没有竖线：列之间三个空格，
//! 表头下一条与表同宽的横线（docs/puffin-tui.md §4.2「回答的 Markdown」）。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Layout.scala 的 `layTable` / `longestWord` / `padAlign`
//! （feat/tui a2bf878b34）。
use crate::render::{Span, StyledLine, attr, width};

use super::super::element::{ColumnAlign, Table, Wrap};
use super::super::table_layout::column_widths;
use super::{prefix_width, wrap};

const SEP_W: usize = 3;

pub(super) fn lay_table(t: &Table, width: usize) -> Vec<StyledLine> {
    let cols = t.align.len();
    if cols == 0 {
        return Vec::new();
    }
    let pfx_w = prefix_width(&t.first_prefix).max(prefix_width(&t.next_prefix));
    let avail = width
        .max(1)
        .saturating_sub(pfx_w + (cols - 1) * SEP_W)
        .max(1);
    let all = || std::iter::once(&t.header).chain(t.rows.iter());

    let cap = (avail / 2).max(1);
    let natural: Vec<usize> = (0..cols)
        .map(|c| {
            all()
                .map(|r| cell_width(cell_at(r, c)))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let mins: Vec<usize> = (0..cols)
        .map(|c| {
            let longest = all()
                .map(|r| longest_word(cell_at(r, c)))
                .max()
                .unwrap_or(0);
            longest.min(cap).max(1)
        })
        .collect();
    let widths = column_widths(&natural, &mins, avail);

    let mut out = Vec::new();
    let mut first_physical = true;
    emit_row(t, &widths, &t.header, &mut first_physical, &mut out);
    // 表头下的横线，与表同宽。
    let mut rule = if first_physical {
        t.first_prefix.clone()
    } else {
        t.next_prefix.clone()
    };
    let rule_w: usize = widths.iter().sum::<usize>() + (cols - 1) * SEP_W;
    rule.push(Span::new("─".repeat(rule_w), t.rule));
    out.push(StyledLine::new(rule));
    first_physical = false;
    for r in &t.rows {
        emit_row(t, &widths, r, &mut first_physical, &mut out);
    }
    out
}

/// 一行格子：各列按词折行，按最高的列出物理行；第一条物理行用第一行前缀。
fn emit_row(
    t: &Table,
    widths: &[usize],
    cells: &[Vec<Span>],
    first_physical: &mut bool,
    out: &mut Vec<StyledLine>,
) {
    let cols = t.align.len();
    let wrapped: Vec<Vec<Vec<Span>>> = (0..cols)
        .map(|c| {
            wrap::wrap_spans(cell_at(cells, c), widths[c], widths[c], Wrap::Word)
                .into_iter()
                .map(|(content, _)| content)
                .collect()
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(0);
    for li in 0..height {
        let mut spans = if *first_physical {
            t.first_prefix.clone()
        } else {
            t.next_prefix.clone()
        };
        for c in 0..cols {
            if c > 0 {
                spans.push(Span::plain(" ".repeat(SEP_W)));
            }
            let cell_line = wrapped[c].get(li).map_or(&[][..], Vec::as_slice);
            pad_align(cell_line, widths[c], t.align[c], &mut spans);
        }
        out.push(StyledLine::new(spans));
        *first_physical = false;
    }
}

fn cell_at(r: &[Vec<Span>], c: usize) -> &[Span] {
    r.get(c).map_or(&[][..], Vec::as_slice)
}

fn cell_width(cell: &[Span]) -> usize {
    cell.iter().map(|s| width::string_width(&s.text)).sum()
}

/// 格子里最宽的、空白分隔的词（显示列）；[`attr::WHOLE`] 样式里的空格不结束词。
fn longest_word(cell: &[Span]) -> usize {
    let (mut best, mut cur) = (0usize, 0usize);
    for sp in cell {
        let whole = sp.style.has_attr(attr::WHOLE);
        for cp in sp.text.chars() {
            if (cp == ' ' && !whole) || cp == '\n' {
                best = best.max(cur);
                cur = 0;
            } else {
                cur += width::display_width(cp);
            }
        }
    }
    best.max(cur)
}

/// 把一行格子（已经不宽于 `width`）按列的对齐补到 `width` 列，接在 `out` 后面。
fn pad_align(spans: &[Span], width: usize, align: ColumnAlign, out: &mut Vec<Span>) {
    let w = cell_width(spans);
    let pad = width.saturating_sub(w);
    let (lp, rp) = match align {
        ColumnAlign::Right => (pad, 0),
        ColumnAlign::Center => (pad / 2, pad - pad / 2),
        ColumnAlign::Left => (0, pad),
    };
    if lp > 0 {
        out.push(Span::plain(" ".repeat(lp)));
    }
    out.extend_from_slice(spans);
    if rp > 0 {
        out.push(Span::plain(" ".repeat(rp)));
    }
}
