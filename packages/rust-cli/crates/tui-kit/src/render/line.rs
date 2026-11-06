//! 物理行：同一样式的一段段文字。布局产出 `Vec<StyledLine>`，渲染器把它们变成格子逐帧比较。
//! 移植自 packages/tui/tui/src/zcode/tui/render/StyledLine.scala（feat/tui a2bf878b34）。
use std::borrow::Cow;

use super::ansi;
use super::style::{Decor, Style};
use super::width;

/// 一段同一视觉样式（与 [`Decor`]）的文字。文字是原文（没有转义序列）；样式在渲染时才应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: Cow<'static, str>,
    pub style: Style,
    pub decor: Decor,
}

impl Span {
    pub fn new(text: impl Into<Cow<'static, str>>, style: Style) -> Span {
        Span {
            text: text.into(),
            style,
            decor: Decor::PLAIN,
        }
    }

    pub fn plain(text: impl Into<Cow<'static, str>>) -> Span {
        Span::new(text, Style::DEFAULT)
    }

    pub fn with_decor(text: impl Into<Cow<'static, str>>, style: Style, decor: Decor) -> Span {
        Span {
            text: text.into(),
            style,
            decor,
        }
    }
}

/// 软折行之后接着上一行文字的行（docs/puffin-tui.md §4.4）：文字从格子 `at` 开始（前面的格子是折行的缩进），
/// `space` 说折行处丢了一个空格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Continuation {
    pub at: usize,
    pub space: bool,
}

/// 一个物理终端行。`cont` 在这一行是上一行的软折行时有值：复制时把它们接起来。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StyledLine {
    pub spans: Vec<Span>,
    pub cont: Option<Continuation>,
}

impl StyledLine {
    pub const EMPTY: StyledLine = StyledLine {
        spans: Vec::new(),
        cont: None,
    };

    pub fn new(spans: Vec<Span>) -> StyledLine {
        StyledLine { spans, cont: None }
    }

    pub fn text(s: impl Into<Cow<'static, str>>, style: Style) -> StyledLine {
        StyledLine::new(vec![Span::new(s, style)])
    }

    /// 整行的显示宽度（列）。
    pub fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|s| width::string_width(&s.text))
            .sum()
    }

    /// 去掉样式的文字。
    pub fn plain(&self) -> String {
        self.spans.iter().map(|s| s.text.as_ref()).collect()
    }

    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }

    /// 一个自含的 ANSI 串：每段从重置设自己的样式，链接在变化处开关，行尾重置（且没有开着的链接），不漏到下一行。
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut link = "";
        for sp in &self.spans {
            if sp.text.is_empty() {
                continue;
            }
            sp.style.append_set_sequence(&mut out);
            out.push_str(&sp.decor.sequence());
            if sp.decor.link != link {
                link = &sp.decor.link;
                if link.is_empty() {
                    out.push_str(ansi::LINK_END);
                } else {
                    out.push_str(&ansi::link_start(link));
                }
            }
            out.push_str(&sp.text);
        }
        if !link.is_empty() {
            out.push_str(ansi::LINK_END);
        }
        out.push_str(ansi::RESET);
        out
    }

    /// 把 `f` 应用到显示格子与 `[from_cell, until_cell)`（0 起的显示列）相交的每个字形的样式上。段在边界处拆开、相邻
    /// 同样式的段重新合并，所以结果的显示宽度与输入相同。跨在边界上的宽字形整个改样式。空或倒着的范围原样返回。
    pub fn restyle_cells(
        &self,
        from_cell: usize,
        until_cell: usize,
        f: impl Fn(Style) -> Style,
    ) -> StyledLine {
        if until_cell <= from_cell {
            return self.clone();
        }
        let mut out: Vec<Span> = Vec::new();
        let mut cur = String::new();
        let mut cur_style = Style::DEFAULT;
        let mut cur_decor = Decor::PLAIN;
        let mut have = false;
        let mut cell = 0usize;
        for sp in &self.spans {
            for cp in sp.text.chars() {
                let w = width::display_width(cp);
                // 字形占 `[cell, cell + max(w, 1))`：与范围重叠时在范围内。`max(w, 1)` 让零宽记号在边界上不被悄悄丢掉。
                let in_range = cell < until_cell && cell + w.max(1) > from_cell;
                let st = if in_range { f(sp.style) } else { sp.style };
                if have && (st != cur_style || sp.decor != cur_decor) {
                    out.push(Span::with_decor(
                        std::mem::take(&mut cur),
                        cur_style,
                        cur_decor.clone(),
                    ));
                }
                cur_style = st;
                cur_decor = sp.decor.clone();
                have = true;
                cur.push(cp);
                cell += w;
            }
        }
        if have {
            out.push(Span::with_decor(cur, cur_style, cur_decor));
        }
        StyledLine {
            spans: out,
            cont: self.cont,
        }
    }
}
