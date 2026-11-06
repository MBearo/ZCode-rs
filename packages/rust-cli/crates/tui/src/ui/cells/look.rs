//! transcript 的块的 Puffin 样子（docs/specs/rust-cli/tui-kit.md §4.1、§4.2，工作线 TK 的 S1）：网格、记号、竖线、量宽与一行
//! 工具行的排法，照 docs/puffin-tui.md §4.2「网格」「工具调用」与 Puffin 的壳（`shell/tool_view.rs` 的 `row`、`rail`、
//! `settled_mark`，`shell/gather.rs` 的 `cut`，`shell/consts.rs` 的 `clip`、`pad`）。块只用这里的东西摆字形，墨色经 `Ink`。
//!
//! 网格（§4.2「网格」）：第 0 列是边距，第 1 列只放记号（`✓` `✗` `–` `○` `●` `•` `!` `›`），第 2 列空，正文从第 3 列起；
//! 正文折行宽度 `min(宽度 − 2, 99)`（`Element::Measure` 的 99 与 2）。挂在一行下面的东西在第 3 列的 line 色 `│` 后面，
//! 从第 5 列起。
use rust_cli_tui_kit::app::element::{
    Element, Wrap, measure as kit_measure, single_line, stack, wrap_styled,
};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::markdown::render::INDENT;
use rust_cli_tui_kit::render::width::{display_width, string_width};
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::Ctx;
use crate::ui::ink::Ink;

/// 正文的最大宽度与右边留的列数（Puffin `consts::MEASURE_MAX`、`MEASURE_MARGIN`）。
pub(crate) const MEASURE_MAX: usize = 99;
pub(crate) const MEASURE_MARGIN: usize = 2;
/// 正文从第 3 列起：边距、记号、空格（kit 的 `render::INDENT`）。
pub(crate) const LEAD: &str = INDENT;

/// 正文的宽度：`min(宽度 − 2, 99)`，至少 1（同 `Element::Measure`）。
pub(crate) fn measure(width: usize) -> usize {
    width.saturating_sub(MEASURE_MARGIN).clamp(1, MEASURE_MAX)
}

/// kit 的元素按 `width` 铺成行。
pub(crate) fn lay(element: &Element, width: usize) -> Vec<StyledLine> {
    Layout::lay(element, width)
}

/// 第 1 列的记号连同它前后的空格：` ✓ `。
pub(crate) fn marked(mark: Span) -> Vec<Span> {
    vec![Span::plain(" "), mark, Span::plain(" ")]
}

/// `   │ `：行下面的竖线（Puffin `tool_view::rail`）。
pub(crate) fn rail(ink: &Ink) -> Vec<Span> {
    vec![
        Span::plain(LEAD),
        Span::new("│", ink.theme.line),
        Span::plain(" "),
    ]
}

/// 竖线上的一段文字：按正文宽度按词折行（kit 的 `Wrap::Word`，与 Puffin 同一个折行），续行也在竖线后面；空行只画竖线
/// （同 Puffin 展开的思考，`shell/view/entries.rs` 的 `thought`）。
pub(crate) fn rail_text(text: &str, style: Style, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let rail = rail(ctx.ink);
    let rows: Vec<Element> = text
        .split('\n')
        .map(|line| {
            let line = line.replace('\r', "").replace('\t', "    ");
            if line.trim().is_empty() {
                single_line(rail[..2].to_vec(), false)
            } else {
                wrap_styled(
                    rail.clone(),
                    rail.clone(),
                    vec![Span::new(line, style)],
                    Wrap::Word,
                )
            }
        })
        .collect();
    lay(&measured(stack(rows)), ctx.width)
}

/// 量过宽的元素：`min(宽度 − 2, 99)`（Puffin 的 `measure(el, MEASURE_MAX, MEASURE_MARGIN)`）。
pub(crate) fn measured(element: Element) -> Element {
    kit_measure(element, MEASURE_MAX, MEASURE_MARGIN)
}

/// `spans` 放进至多 `room` 格：超出时截断并以 `…` 结尾，截断处之前的墨色不变（Puffin `gather::cut`）。
pub(crate) fn cut(spans: &[Span], room: i64) -> Vec<Span> {
    if spans_width(spans) as i64 <= room {
        return spans.to_vec();
    }
    let mut out = Vec::new();
    let mut used: i64 = 0;
    for sp in spans {
        let w = string_width(&sp.text) as i64;
        if used + w < room {
            out.push(sp.clone());
            used += w;
        } else {
            let mut text = String::new();
            for ch in sp.text.chars() {
                let cw = display_width(ch) as i64;
                if used + cw > room - 1 {
                    break;
                }
                text.push(ch);
                used += cw;
            }
            text.push('…');
            out.push(Span::with_decor(text, sp.style, sp.decor.clone()));
            break;
        }
    }
    out
}

/// 段的总宽度（格）。
pub(crate) fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// `text` 截到 `cols` 格，截断时最后一格是 `…`（Puffin `consts::clip`）。
pub(crate) fn clip(text: &str, cols: usize) -> String {
    if string_width(text) <= cols {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = display_width(ch);
        if w + cw < cols {
            out.push(ch);
            w += cw;
        }
    }
    out.push('…');
    out
}
