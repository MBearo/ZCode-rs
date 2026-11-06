//! 把视图 DSL 铺成给渲染器的样式行；也是把 `Text` 里嵌的 ANSI 重新切成结构化段的唯一地方（应用可以继续用子元素的
//! `render` 拼字符串）。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Layout.scala（feat/tui a2bf878b34）。
//!
//! 输出与 Scala 逐行相同（画面门禁）。文字按码点（`char`）走，不按 UTF-16
//! 单元；宽度用 [`width::display_width`]。几处 Scala 按 UTF-16 单元截取的地方（标签与裁剪标记的 `take`）这里按码点截，
//! 只在辅助平面的字符上有差别。
//!
//! 子模块：`wrap`（折行引擎）、`sgr`（嵌入 SGR 的重新切段）、`clip`（竖向裁剪）、`table`（表格）。
mod clip;
mod sgr;
mod table;
mod wrap;

use std::borrow::Cow;
use std::sync::Arc;

use crate::render::{Continuation, Span, Style, StyledLine, width};

use super::element::{CodePanel, Element, Memo, Wrap};

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// 不换行空格：[`Element::Caret`] 找末尾一串它的第一格。
const NBSP: char = '\u{a0}';

/// 布局。没有状态：记忆在 [`Memo`] 里。
pub struct Layout;

impl Layout {
    /// 按 `width` 列把元素铺成物理行。
    pub fn lay(body: &Element, width: usize) -> Vec<StyledLine> {
        let mut out = Vec::new();
        lay_into(body, width, &mut out);
        out
    }

    /// 同 [`Layout::lay`]，但拿走元素：[`Element::Raw`]（整帧）的行直接移出，不复制。
    pub fn into_lines(body: Element, width: usize) -> Vec<StyledLine> {
        match body {
            Element::Raw(lines) => lines,
            other => Layout::lay(&other, width),
        }
    }

    /// 一个记忆节点在 `width` 下的行，共享记着的那一份（命中时不复制）。宽度或
    /// [`width::generation`] 变了才重铺。壳可以按条目持有这些 `Arc`，不必每帧把整份 transcript 复制进一个 `Vec`。
    pub fn lay_memo(memo: &Memo, width: usize) -> Arc<Vec<StyledLine>> {
        let generation = width::generation();
        if let Some(lines) = memo.cached(width, generation) {
            return lines;
        }
        let lines = Arc::new(Layout::lay(memo.inner(), width));
        memo.store(width, generation, Arc::clone(&lines));
        lines
    }

    /// 渲染成换行连接的 ANSI 串，给行内拼接用（按 80 列铺）。
    pub fn render_inline(body: &Element) -> String {
        Layout::lay(body, 80)
            .iter()
            .map(StyledLine::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 把 `e` 铺进 `out` 的末尾：竖向堆叠不造中间的 `Vec`。
fn lay_into(e: &Element, width: usize, out: &mut Vec<StyledLine>) {
    match e {
        Element::Text(value, style) => {
            for seg in split_lines(value) {
                out.push(StyledLine::new(sgr::tokenize(seg, *style)));
            }
        }
        Element::Stack(children) => {
            for c in children {
                lay_into(c, width, out);
            }
        }
        Element::LineBreak => out.push(StyledLine::EMPTY),
        Element::Blank => {}
        Element::Rule(ch, style) => {
            let text: String = std::iter::repeat_n(*ch, width.max(1)).collect();
            out.push(StyledLine::text(text, *style));
        }
        Element::LabelledRule(label, style) => out.push(labelled_rule(label, *style, width)),
        Element::RoundBox(inner, style) => lay_round_box(inner, *style, width, out),
        Element::Spinner {
            label,
            frame,
            style,
        } => {
            let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
            out.push(StyledLine::text(format!("{glyph} {label}"), *style));
        }
        Element::Styled(inner, style) => {
            let start = out.len();
            lay_into(inner, width, out);
            for line in &mut out[start..] {
                for sp in &mut line.spans {
                    sp.style = style.merge(sp.style);
                }
            }
        }
        Element::WrappedText {
            first_prefix,
            next_prefix,
            value,
            style,
            mode,
        } => {
            let first = sgr::tokenize(first_prefix.clone(), Style::DEFAULT);
            let next = sgr::tokenize(next_prefix.clone(), Style::DEFAULT);
            let spans = sgr::tokenize(value.clone(), *style);
            lay_wrapped(&first, &next, &spans, width, *mode, out);
        }
        Element::WrappedSpans {
            first_prefix,
            next_prefix,
            spans,
            mode,
        } => lay_wrapped(first_prefix, next_prefix, spans, width, *mode, out),
        Element::Memo(memo) => {
            out.extend(Layout::lay_memo(memo, width).iter().cloned());
        }
        Element::Table(t) => out.extend(table::lay_table(t, width)),
        Element::Clip(c) => {
            let rows = Layout::lay(&c.inner, width);
            out.extend(clip::lay_clipped(rows, c, width));
        }
        Element::Raw(lines) => out.extend(lines.iter().cloned()),
        Element::Slab(inner, bg) => {
            let w = width.max(1);
            out.push(StyledLine::text("▄".repeat(w), Style::fg(*bg)));
            for line in Layout::lay(inner, w) {
                let pad = w.saturating_sub(line.width());
                let mut spans: Vec<Span> =
                    line.spans.into_iter().map(|sp| on_bg(sp, *bg)).collect();
                if pad > 0 {
                    spans.push(Span::new(" ".repeat(pad), Style::bg(*bg)));
                }
                out.push(StyledLine {
                    spans,
                    cont: line.cont,
                });
            }
            out.push(StyledLine::text("▀".repeat(w), Style::fg(*bg)));
        }
        Element::CodePanel(panel) => lay_code_panel(panel, width, out),
        Element::Measure { inner, max, margin } => {
            lay_into(inner, width.saturating_sub(*margin).min(*max).max(1), out);
        }
        Element::Spread(left, right) => {
            let gap = width
                .saturating_sub(prefix_width(left) + prefix_width(right))
                .max(2);
            let mut spans = left.clone();
            spans.push(Span::plain(" ".repeat(gap)));
            spans.extend(right.iter().cloned());
            out.push(StyledLine::new(spans));
        }
        Element::FirstFit(variants) => lay_first_fit(variants, width, out),
        Element::Line { spans, keep_end } => out.push(lay_line(spans, *keep_end, width)),
        Element::Caret(inner, caret) => {
            let start = out.len();
            lay_into(inner, width, out);
            // 内部铺出了行：光标放在它的最后一行；没有行：光标自己一行。
            let last = if out.len() > start { out.pop() } else { None };
            match last {
                Some(last) => out.push(with_caret(last, caret)),
                None => out.push(StyledLine::new(caret.clone())),
            }
        }
        Element::ByWidth(f) => lay_into(&(f.0)(width), width, out),
        Element::Map(inner, f) => out.extend((f.0)(Layout::lay(inner, width))),
        Element::Beside {
            left,
            left_width,
            right,
        } => lay_beside(left, *left_width, right, width, out),
    }
}

/// 按 `\n` 拆行（保留末尾的空段，Scala 的 `split("\n", -1)`）。借来的 `'static` 文字切出的仍是借用。
#[expect(
    clippy::ptr_arg,
    reason = "要分清借用与拥有：借来的 'static 文字切出借用，不复制"
)]
fn split_lines(value: &Cow<'static, str>) -> Vec<Cow<'static, str>> {
    if !value.contains('\n') {
        return vec![value.clone()];
    }
    match value {
        Cow::Borrowed(s) => {
            let s: &'static str = s;
            s.split('\n').map(Cow::Borrowed).collect()
        }
        Cow::Owned(s) => s.split('\n').map(|p| Cow::Owned(p.to_owned())).collect(),
    }
}

/// 底色还是默认的段铺到 `bg` 上；自己有底色的不动。
fn on_bg(sp: Span, bg: crate::render::Color) -> Span {
    if sp.style.bg_color().is_default() {
        Span {
            style: sp.style.with_bg(bg),
            ..sp
        }
    } else {
        sp
    }
}

/// 两侧 `─`，离标签一个空格；标签自己就占满一行时没有横线的位置，退化成（截断的）标签。
fn labelled_rule(label: &str, style: Style, width: usize) -> StyledLine {
    let w = width.max(1);
    let lw = width::string_width(label);
    let text = if lw + 4 <= w {
        let fill = w - lw - 2;
        let left = fill / 2;
        format!("{} {label} {}", "─".repeat(left), "─".repeat(fill - left))
    } else {
        label.chars().take(w).collect()
    };
    StyledLine::text(text, style)
}

/// 框每行占 4 列（`│ ` 与 ` │`），四角是圆的。
fn lay_round_box(inner: &Element, style: Style, width: usize, out: &mut Vec<StyledLine>) {
    let w = width.max(5);
    let edge = "─".repeat(w - 2);
    let inner_w = w - 4;
    out.push(StyledLine::text(format!("╭{edge}╮"), style));
    for line in Layout::lay(inner, inner_w) {
        let pad = inner_w.saturating_sub(line.width());
        let mut spans = Vec::with_capacity(line.spans.len() + 3);
        spans.push(Span::new("│ ", style));
        spans.extend(line.spans);
        if pad > 0 {
            spans.push(Span::plain(" ".repeat(pad)));
        }
        spans.push(Span::new(" │", style));
        out.push(StyledLine {
            spans,
            cont: line.cont.map(|c| Continuation { at: c.at + 2, ..c }),
        });
    }
    out.push(StyledLine::text(format!("╰{edge}╯"), style));
}

fn lay_code_panel(p: &CodePanel, width: usize, out: &mut Vec<StyledLine>) {
    let pfx_w = prefix_width(&p.prefix);
    let w = width.saturating_sub(pfx_w).max(3);
    let fill = Style::bg(p.bg);
    let label_w = prefix_width(&p.label);
    // 折开的代码行接在前缀与面板的一格留白之后。
    let text_at = pfx_w + 1;
    let edge = |ch: &str| {
        let mut spans = p.prefix.clone();
        spans.push(Span::new(ch.repeat(w), Style::fg(p.bg)));
        StyledLine::new(spans)
    };
    out.push(edge("▄"));
    let mut i = 0usize;
    for l in &p.lines {
        let on_panel: Vec<Span> = l.iter().cloned().map(|sp| on_bg(sp, p.bg)).collect();
        for (content, mark) in wrap::wrap_spans(&on_panel, w - 2, w - 2, Wrap::Char) {
            let used = 1 + prefix_width(&content);
            let mut spans = p.prefix.clone();
            spans.push(Span::new(" ", fill));
            spans.extend(content);
            if i == 0 && label_w > 0 && used + label_w + 2 <= w {
                spans.push(Span::new(" ".repeat(w - used - label_w - 1), fill));
                spans.extend(p.label.iter().map(|sp| Span {
                    style: sp.style.with_bg(p.bg),
                    ..sp.clone()
                }));
                spans.push(Span::new(" ", fill));
            } else {
                spans.push(Span::new(" ".repeat(w - used), fill));
            }
            out.push(StyledLine {
                spans,
                cont: mark.map(|space| Continuation { at: text_at, space }),
            });
            i += 1;
        }
    }
    if !p.open {
        out.push(edge("▀"));
    }
}

/// 第一个每行都放得下的变体；都不行时最后一个，每行切到宽度。
fn lay_first_fit(variants: &[Element], width: usize, out: &mut Vec<StyledLine>) {
    let w = width.max(1);
    let mut last = Vec::new();
    for v in variants {
        let ls = Layout::lay(v, w);
        if ls.iter().all(|l| l.width() <= w) {
            out.extend(ls);
            return;
        }
        last = ls;
    }
    out.extend(last.into_iter().map(|l| StyledLine {
        spans: cut(&l.spans, w),
        cont: l.cont,
    }));
}

fn lay_line(spans: &[Span], keep_end: bool, width: usize) -> StyledLine {
    let w = width.max(1);
    if prefix_width(spans) <= w {
        StyledLine::new(spans.to_vec())
    } else if keep_end {
        // 从开头切：把段与段里的文字倒过来，切，再倒回去。
        let mirror = |v: &[Span]| -> Vec<Span> {
            v.iter()
                .rev()
                .map(|sp| Span {
                    text: Cow::Owned(sp.text.chars().rev().collect()),
                    ..sp.clone()
                })
                .collect()
        };
        let kept = mirror(&cut(&mirror(spans), w - 1));
        let style = kept.first().map_or(Style::DEFAULT, |sp| sp.style);
        let mut out = vec![Span::new("…", style)];
        out.extend(kept);
        StyledLine::new(out)
    } else {
        let mut kept = cut(spans, w - 1);
        let style = kept.last().map_or(Style::DEFAULT, |sp| sp.style);
        kept.push(Span::new("…", style));
        StyledLine::new(kept)
    }
}

fn lay_beside(
    left: &Element,
    left_width: usize,
    right: &Element,
    width: usize,
    out: &mut Vec<StyledLine>,
) {
    let l = Layout::lay(left, left_width.max(1));
    let r = Layout::lay(right, width.saturating_sub(left_width).max(1));
    let n = l.len().max(r.len());
    let mut l = l.into_iter();
    let mut r = r.into_iter();
    for _ in 0..n {
        let ll = l.next().unwrap_or_default();
        let rr = r.next().unwrap_or_default();
        let lw = ll.width();
        let pad = left_width.saturating_sub(lw);
        let mut spans = ll.spans;
        spans.push(Span::plain(" ".repeat(pad)));
        spans.extend(rr.spans);
        // 右列的软折行就是这一行的：它的文字在左列之后开始。
        out.push(StyledLine {
            spans,
            cont: rr.cont.map(|c| Continuation {
                at: c.at + lw + pad,
                ..c
            }),
        });
    }
}

/// `line` 带上 `caret`：从末尾一串不换行空格的第一格起，没有就接在后面。
fn with_caret(line: StyledLine, caret: &[Span]) -> StyledLine {
    let n = line
        .spans
        .iter()
        .rev()
        .flat_map(|sp| sp.text.chars().rev())
        .take_while(|&c| c == NBSP)
        .count();
    if n == 0 {
        let mut spans = line.spans;
        spans.extend_from_slice(caret);
        StyledLine {
            spans,
            cont: line.cont,
        }
    } else {
        let mut spans = cut(&line.spans, line.width() - n);
        spans.extend_from_slice(caret);
        // 还没显示的字占着的格子被光标盖住多少，就少留多少。
        let wide: usize = caret.iter().map(|sp| width::string_width(&sp.text)).sum();
        spans.push(Span::plain(NBSP.to_string().repeat(n.saturating_sub(wide))));
        StyledLine {
            spans,
            cont: line.cont,
        }
    }
}

/// `spans` 不宽于 `w` 格的最长前缀（跨在边上的宽字形整个丢掉）。
fn cut(spans: &[Span], w: usize) -> Vec<Span> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for sp in spans {
        let sw = width::string_width(&sp.text);
        if used + sw <= w {
            out.push(sp.clone());
            used += sw;
            continue;
        }
        let mut kept = String::new();
        for cp in sp.text.chars() {
            let cw = width::display_width(cp);
            if used + cw > w {
                break;
            }
            kept.push(cp);
            used += cw;
        }
        if !kept.is_empty() {
            out.push(Span {
                text: Cow::Owned(kept),
                ..sp.clone()
            });
        }
        break;
    }
    out
}

/// 在第一行前缀与后续行前缀下软折行。
fn lay_wrapped(
    first: &[Span],
    next: &[Span],
    spans: &[Span],
    width: usize,
    mode: Wrap,
    out: &mut Vec<StyledLine>,
) {
    let full_width = width.max(1);
    let first_width = full_width.saturating_sub(prefix_width(first)).max(1);
    let rest_width = full_width.saturating_sub(prefix_width(next)).max(1);
    let next_width = prefix_width(next);
    for (i, (content, mark)) in wrap::wrap_spans(spans, first_width, rest_width, mode)
        .into_iter()
        .enumerate()
    {
        let mut line = if i == 0 {
            first.to_vec()
        } else {
            next.to_vec()
        };
        line.extend(content);
        out.push(StyledLine {
            spans: line,
            cont: mark.map(|space| Continuation {
                at: next_width,
                space,
            }),
        });
    }
}

fn prefix_width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| width::string_width(&sp.text)).sum()
}
