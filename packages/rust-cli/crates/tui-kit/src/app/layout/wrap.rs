//! 共用的折行引擎：把样式段重排成内容行（不带前缀），每行不宽于它的预算（第 0 行 `first_width`，之后 `rest_width`）。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Layout.scala 的 `wrapSpans` / `WordWrap`（feat/tui a2bf878b34）。
//!
//! [`Wrap::Word`] 在词之间折（一个词自己就超出一行时才拆开），并在宽字符前后折：宽字符自己是一个词；[`Wrap::Char`]
//! 在任何码点处折。样式边界与码点宽度保留；显式的 `\n` 总是折。每行带标记：从软折行开始时是 `Some(space)`，`space`
//! 说折行处丢了一个空格；第一行与 `\n` 之后是 `None`。
//!
//! 先把码点、宽度与来处（哪个段的哪个字节）各存一个数组，连续同样式（与 Decor）的码点攒成一段，段结束时按来处切子串、
//! 一次成 Span（Scala 的渲染基准修复，docs/puffin-tui.md §7「渲染基准（L0）」）。一段正好是某个源段的全文时直接复用
//! 它的 `Cow`（借来的 `'static` 文字不复制）。
use std::borrow::Cow;

use crate::render::{Decor, Span, Style, attr, width};

use super::super::element::Wrap;

/// 一行内容与它的软折行标记。
pub(super) type WrappedLine = (Vec<Span>, Option<bool>);

pub(super) fn wrap_spans(
    spans: &[Span],
    first_width: usize,
    rest_width: usize,
    mode: Wrap,
) -> Vec<WrappedLine> {
    WordWrap::new(spans, first_width, rest_width).wrap(mode)
}

/// 一次折行，状态放在字段里。
struct WordWrap<'a> {
    spans: &'a [Span],
    cps: Vec<char>,
    ws: Vec<usize>,
    span_of: Vec<usize>,
    /// 码点在源段文字里的字节偏移。
    off_of: Vec<usize>,
    rest_width: usize,

    out: Vec<WrappedLine>,
    line: Vec<Span>,
    col: usize,
    limit: usize,
    /// 正在造的这一行的标记。
    mark: Option<bool>,
    /// 最后追加的码点。
    last: Option<char>,
    // 开着的一段：正在造的 Span，文字是已折进来的部分（`run_text`）加上段 `run_span` 里还在长的 [run_from, run_to)。
    run_open: bool,
    run_style: Style,
    run_decor: Decor,
    run_text: String,
    run_span: Option<usize>,
    run_from: usize,
    run_to: usize,
}

impl<'a> WordWrap<'a> {
    fn new(spans: &'a [Span], first_width: usize, rest_width: usize) -> WordWrap<'a> {
        let n: usize = spans.iter().map(|sp| sp.text.chars().count()).sum();
        let mut cps = Vec::with_capacity(n);
        let mut ws = Vec::with_capacity(n);
        let mut span_of = Vec::with_capacity(n);
        let mut off_of = Vec::with_capacity(n);
        for (s, sp) in spans.iter().enumerate() {
            for (k, cp) in sp.text.char_indices() {
                cps.push(cp);
                ws.push(width::display_width(cp));
                span_of.push(s);
                off_of.push(k);
            }
        }
        WordWrap {
            spans,
            cps,
            ws,
            span_of,
            off_of,
            rest_width,
            out: Vec::new(),
            line: Vec::new(),
            col: 0,
            limit: first_width,
            mark: None,
            last: None,
            run_open: false,
            run_style: Style::DEFAULT,
            run_decor: Decor::PLAIN,
            run_text: String::new(),
            run_span: None,
            run_from: 0,
            run_to: 0,
        }
    }

    /// 把还在长的一片折进这一段的文字。
    fn piece(&mut self) {
        if let Some(s) = self.run_span
            && self.run_to > self.run_from
        {
            self.run_text
                .push_str(&self.spans[s].text[self.run_from..self.run_to]);
        }
        self.run_span = None;
    }

    fn flush_run(&mut self) {
        if !self.run_open {
            return;
        }
        let text: Cow<'static, str> = match self.run_span {
            // 只有一片：从源段切，整段时复用源段的 `Cow`。
            Some(s) if self.run_text.is_empty() => {
                let (from, to) = (self.run_from, self.run_to);
                match &self.spans[s].text {
                    Cow::Borrowed(b) => {
                        let b: &'static str = b;
                        Cow::Borrowed(&b[from..to])
                    }
                    src @ Cow::Owned(o) => {
                        if from == 0 && to == o.len() {
                            src.clone()
                        } else {
                            Cow::Owned(o[from..to].to_owned())
                        }
                    }
                }
            }
            _ => {
                self.piece();
                Cow::Owned(std::mem::take(&mut self.run_text))
            }
        };
        self.run_span = None;
        self.line.push(Span::with_decor(
            text,
            self.run_style,
            std::mem::take(&mut self.run_decor),
        ));
        self.run_open = false;
    }

    /// 结束这一行；`next` 是下一行的标记。
    fn emit(&mut self, next: Option<bool>) {
        self.flush_run();
        self.out.push((std::mem::take(&mut self.line), self.mark));
        self.col = 0;
        self.limit = self.rest_width;
        self.mark = next;
    }

    fn append_one(&mut self, i: usize) {
        let s = self.span_of[i];
        let sp = &self.spans[s];
        let from = self.off_of[i];
        let to = from + self.cps[i].len_utf8();
        if !self.run_open || self.run_style != sp.style || self.run_decor != sp.decor {
            self.flush_run();
            self.run_open = true;
            self.run_style = sp.style;
            self.run_decor = sp.decor.clone();
            self.run_span = Some(s);
            self.run_from = from;
            self.run_to = to;
        } else if self.run_span == Some(s) && self.run_to == from {
            self.run_to = to;
        } else {
            self.piece();
            self.run_span = Some(s);
            self.run_from = from;
            self.run_to = to;
        }
        self.col += self.ws[i];
        self.last = Some(self.cps[i]);
    }

    fn add_range(&mut self, a: usize, b: usize) {
        for i in a..b {
            self.append_one(i);
        }
    }

    fn range_width(&self, a: usize, b: usize) -> usize {
        self.ws[a..b].iter().sum()
    }

    fn char_break(&mut self, a: usize, b: usize) {
        for i in a..b {
            let w = self.ws[i];
            if w > 0 && self.col > 0 && self.col + w > self.limit {
                self.emit(Some(self.last == Some(' ')));
            }
            self.append_one(i);
        }
    }

    /// 带样式（非默认）的空格是原子的：它是输入框的光标格，在折开的行尾也要留下。[`attr::WHOLE`] 样式里的空格属于
    /// 它的词：不在那里折。
    fn breaks(&self, i: usize) -> bool {
        self.cps[i] == ' ' && !self.spans[self.span_of[i]].style.has_attr(attr::WHOLE)
    }

    fn gap_has_styled_space(&self, a: usize, b: usize) -> bool {
        (a..b).any(|i| self.spans[self.span_of[i]].style != Style::DEFAULT)
    }

    fn wrap(mut self, mode: Wrap) -> Vec<WrappedLine> {
        let n = self.cps.len();
        match mode {
            Wrap::Char => {
                for i in 0..n {
                    if self.cps[i] == '\n' {
                        self.emit(None);
                    } else {
                        self.char_break(i, i + 1);
                    }
                }
            }
            Wrap::Word => {
                let mut i = 0usize;
                // 一串待定的可折叠空格。
                let (mut gap_a, mut gap_b, mut gap_w) = (0usize, 0usize, 0usize);
                while i < n {
                    if self.cps[i] == '\n' {
                        // 硬换行处折叠行尾的普通空格，但留下带样式的空格：停在这一行末尾的光标格。
                        if gap_w > 0 && self.gap_has_styled_space(gap_a, gap_b) {
                            self.add_range(gap_a, gap_b);
                        }
                        gap_w = 0;
                        self.emit(None);
                        i += 1;
                    } else if self.breaks(i) {
                        gap_a = i;
                        while i < n && self.breaks(i) {
                            i += 1;
                        }
                        gap_b = i;
                        gap_w = gap_b - gap_a;
                    } else {
                        // 宽字符（中日韩）自己是一个词：以前一段没有空格的中文算一个词，放不下时整段换到下一行，流式输出时
                        // 它先写在这一行、长过行尾又整段跳下去（§4.2「流式节奏」的「词不移动」）。
                        let a = i;
                        if self.ws[i] >= 2 {
                            i += 1;
                        } else {
                            while i < n && !self.breaks(i) && self.cps[i] != '\n' && self.ws[i] < 2
                            {
                                i += 1;
                            }
                        }
                        let ww = self.range_width(a, i);
                        if self.col == 0 {
                            if ww <= self.limit {
                                self.add_range(a, i);
                            } else {
                                self.char_break(a, i);
                            }
                        } else if self.col + gap_w + ww <= self.limit {
                            self.add_range(gap_a, gap_b);
                            self.add_range(a, i);
                        } else {
                            self.emit(Some(gap_w > 0));
                            if ww <= self.limit {
                                self.add_range(a, i);
                            } else {
                                self.char_break(a, i);
                            }
                        }
                        // 两个词之间可以没有空格（宽字符），下一个词不能再加上这一段空格。
                        gap_a = i;
                        gap_b = i;
                        gap_w = 0;
                    }
                }
                // 冲掉末尾的一串空格（比如输入框的光标格）。
                if gap_w > 0 {
                    self.add_range(gap_a, gap_b);
                }
            }
        }
        self.flush_run();
        self.out.push((std::mem::take(&mut self.line), self.mark));
        self.out
    }
}
