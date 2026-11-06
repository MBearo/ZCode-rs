//! 一屏（docs/specs/rust-cli/tui-kit.md §3.2）：`draw::draw` 排出正好一屏的 kit 行，runner 交给 kit 的渲染器。叠在上面的
//! 几处（复制的提示、回到底部的标签、背景标志、悬停、全屏视图的选区）都在排好的行上按显示列改：这里是那几个按列的小操作。宽度只经 kit 的 `render::width`，与渲染器的格子一致。
use rust_cli_tui_kit::render::renderer::Backdrop;
use rust_cli_tui_kit::render::width::display_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

/// `draw::draw` 的结果：`lines` 正好是终端的行数，每行不超过终端的宽度；
/// `backdrop` 是主题涂的地（`Theme.paints` 时）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub lines: Vec<StyledLine>,
    pub backdrop: Option<Backdrop>,
}

/// 行补到 `width` 列（默认样式的空格）；已经够宽时不动。
pub fn pad(line: &mut StyledLine, width: usize) {
    let used = line.width();
    if used < width {
        line.spans.push(Span::plain(" ".repeat(width - used)));
    }
}

/// 按显示列把行切成 `[0, col)` 与 `[col, ..)`；跨在 `col` 上的宽字形换成空格（左边一格、右边一格），两边的宽度都对。
/// 行不够 `col` 列时左边补空格。`cont` 留在左边。
pub fn split(line: &StyledLine, col: usize) -> (StyledLine, StyledLine) {
    let mut left = StyledLine {
        spans: Vec::new(),
        cont: line.cont,
    };
    let mut right = StyledLine::default();
    let mut cell = 0usize;
    for span in &line.spans {
        let (mut l, mut r) = (String::new(), String::new());
        for c in span.text.chars() {
            let w = display_width(c);
            if cell + w <= col {
                l.push(c);
            } else if cell >= col {
                r.push(c);
            } else {
                // 宽字形跨在边界上：两边各一格空格。
                l.push(' ');
                r.push(' ');
            }
            cell += w;
        }
        if !l.is_empty() {
            left.spans
                .push(Span::with_decor(l, span.style, span.decor.clone()));
        }
        if !r.is_empty() {
            right
                .spans
                .push(Span::with_decor(r, span.style, span.decor.clone()));
        }
    }
    pad(&mut left, col);
    (left, right)
}

/// 把 `piece` 盖在 `line` 的第 `col` 列起（原来往矩形里画一段 `Paragraph`）：之前的列与 `piece` 之后的列照旧。
pub fn overlay(line: &StyledLine, col: usize, piece: StyledLine) -> StyledLine {
    let (mut out, rest) = split(line, col);
    let (_, after) = split(&rest, piece.width());
    out.spans.extend(piece.spans);
    if after.width() > 0 {
        out.spans.extend(after.spans);
    }
    out
}

/// 给第 `col` 列的格子加 [`attr::CARET`]（tui-kit.md §3.2「底部的光标」）：渲染器把隐藏的终端光标停在那里，光标格是
/// Puffin 的反显格。格子在行尾之后时补空格到那一列再放一个带 CARET 的空格。
pub fn caret(line: &mut StyledLine, col: usize) {
    if line.width() <= col {
        pad(line, col);
        line.spans.push(Span::new(" ", Style::CARET));
        return;
    }
    *line = line.restyle_cells(col, col + 1, |style| {
        style.with_attr(attr::CARET | attr::REVERSE)
    });
}

#[cfg(test)]
mod tests {
    use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

    use super::{caret, overlay, split};

    fn line(text: &str) -> StyledLine {
        StyledLine::new(vec![Span::plain(text.to_owned())])
    }

    #[test]
    fn split_keeps_both_widths_across_a_wide_glyph() {
        let (l, r) = split(&line("a中b"), 2);
        assert_eq!((l.plain(), r.plain()), ("a ".to_owned(), " b".to_owned()));
        let (l, r) = split(&line("ab"), 4);
        assert_eq!((l.plain(), r.plain()), ("ab  ".to_owned(), String::new()));
    }

    #[test]
    fn overlay_replaces_only_the_covered_columns() {
        let out = overlay(&line("abcdefgh"), 2, line("XY"));
        assert_eq!(out.plain(), "abXYefgh");
        let out = overlay(&line("ab"), 4, line("XY"));
        assert_eq!(out.plain(), "ab  XY");
    }

    #[test]
    fn the_caret_marks_one_cell_or_pads_past_the_end() {
        let mut l = line("abc");
        caret(&mut l, 1);
        let marked: Vec<_> = l
            .spans
            .iter()
            .filter(|s| s.style.has_attr(attr::CARET))
            .map(|s| s.text.to_string())
            .collect();
        assert_eq!(marked, ["b"]);
        let mut l = line("ab");
        caret(&mut l, 4);
        assert_eq!(l.plain(), "ab   ");
        assert_eq!(l.spans.last().unwrap().style, Style::CARET);
    }
}
