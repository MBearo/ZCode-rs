//! 一个块 → Element：段落、标题、引用、横线、代码（面板或细栏）、列表、表格，以及流式回答末尾那个开放的块。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/render/MarkdownRender.scala（feat/tui a2bf878b34）。
use crate::app::element::{
    ColumnAlign, Element, Wrap, beside, caret_at, code_panel, empty, hr, single_line, stack, table,
    wrap_styled,
};
use crate::markdown::ast::{Align, Block, Inline, ListItem};
use crate::markdown::syntax::{self, Kind};
use crate::render::{Color, Span, Style, attr};

use super::inline::{Run, cut_hidden};
use super::{HIDDEN, INDENT, Md, Tail, piped, width_of};

/// 一块在哪里：第一行与之后各行的前缀、在几层列表里、文字的墨（引用里 fg2，做完的任务 fg3）。
#[derive(Clone, Debug)]
pub(super) struct At {
    pub(super) first: Vec<Span>,
    pub(super) cont: Vec<Span>,
    pub(super) depth: usize,
    pub(super) ink: Style,
}

impl At {
    /// 顶层：内容从第 3 列开始。
    pub(super) fn top() -> At {
        At {
            first: vec![Span::plain(INDENT)],
            cont: vec![Span::plain(INDENT)],
            depth: 0,
            ink: Style::DEFAULT,
        }
    }

    /// 第一块之后的块在同一个地方：每一行都是接续。
    fn rest(&self) -> At {
        At {
            first: self.cont.clone(),
            ..self.clone()
        }
    }
}

fn space() -> Span {
    Span::plain(" ")
}

fn joined(a: &[Span], b: impl IntoIterator<Item = Span>) -> Vec<Span> {
    a.iter().cloned().chain(b).collect()
}

const BULLETS: [&str; 3] = ["•", "◦", "▪"];

impl Md {
    fn quote_rail(&self) -> [Span; 2] {
        [Span::new("│", self.theme.line), space()]
    }

    fn code_rail(&self) -> [Span; 2] {
        [Span::new("▏", self.theme.line), space()]
    }

    /// 每块一个元素；第一块从 `at` 开始，其余的接续它。
    pub(super) fn render_list(&self, blocks: &[Block], at: &At) -> Vec<Element> {
        blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if i == 0 {
                    self.render_block(b, at)
                } else {
                    self.render_block(b, &at.rest())
                }
            })
            .collect()
    }

    pub(super) fn render_block(&self, b: &Block, at: &At) -> Element {
        match b {
            Block::Paragraph(content) => {
                let runs = self.paragraph_runs(content, at.ink);
                wrap_styled(
                    at.first.clone(),
                    at.cont.clone(),
                    self.spans(runs, true),
                    Wrap::Word,
                )
            }
            Block::Heading { level, content } => {
                let runs = self.inline_runs(content, at.ink.merge(self.heading_style(*level)));
                wrap_styled(
                    at.first.clone(),
                    at.cont.clone(),
                    self.spans(runs, true),
                    Wrap::Word,
                )
            }
            Block::Code { lang, code, .. } => {
                self.render_code(lang.as_deref(), code, at, &[], false)
            }
            Block::Quote(inner) => {
                let elems = self.render_list(inner, &self.quoted(at));
                self.join_blocks(inner.iter().zip(elems).collect(), true)
            }
            // 从内容缩进画到行宽末端的一条细线。
            Block::ThematicBreak => beside(
                single_line(at.first.clone(), false),
                width_of(&at.first),
                hr('─', Color::Default).style(self.theme.line),
            ),
            Block::List { .. } => self.render_list_block(b, at, None),
            Block::Table { .. } => self.render_table(b, at),
        }
    }

    /// 流式回答末尾的块：[`HIDDEN`] 之后的字画成空白，笔尖在下一个词要去的地方。列表与引用把它传给自己的最后一块；
    /// 开着的代码块写到播放头为止、不画下边；还在到达的表格不画（列宽取决于每一行），可能是表头的段落也不画：
    /// 笔尖停在它的位置。
    pub(super) fn render_open(&self, b: &Block, at: &At, tail: &Tail) -> Element {
        match b {
            Block::Paragraph(content) if piped(content) && !tail.ended => {
                // 以前表头在 `|---|` 行到达前是段落：先按原样打出 `| A | B |`，分隔行一到又整行消失。
                // 第一行以 `|` 开头的段落还在到达时先不画，结束时还不是表格就照常画成段落（§4.2「流式节奏」的「表格」）。
                self.resting_nib(at, tail)
            }
            Block::Paragraph(content) => self.prose(at, self.paragraph_runs(content, at.ink), tail),
            Block::Heading { level, content } => {
                let runs = self.inline_runs(content, at.ink.merge(self.heading_style(*level)));
                self.prose(at, runs, tail)
            }
            Block::List { .. } => self.render_list_block(b, at, Some(tail)),
            Block::Quote(inner) => match inner.split_last() {
                Some((last, init)) => {
                    let q = self.quoted(at);
                    let mut elems = self.render_list(init, &q);
                    let last_at = if init.is_empty() { q } else { q.rest() };
                    elems.push(self.render_open(last, &last_at, tail));
                    self.join_blocks(inner.iter().zip(elems).collect(), true)
                }
                None => self.render_block(b, at),
            },
            Block::Code { lang, code, .. } => {
                let shown = code.split(HIDDEN).next().unwrap_or("");
                self.render_code(lang.as_deref(), shown, at, &tail.nib, !tail.ended)
            }
            Block::Table { .. } => {
                if tail.ended {
                    self.render_table(b, at)
                } else {
                    self.resting_nib(at, tail)
                }
            }
            other => self.render_block(other, at),
        }
    }

    /// 不画的块的位置上只有笔尖（没有笔尖就什么都不画）。
    fn resting_nib(&self, at: &At, tail: &Tail) -> Element {
        if tail.nib.is_empty() {
            return empty();
        }
        single_line(joined(&at.first, tail.nib.iter().cloned()), false)
    }

    fn prose(&self, at: &At, runs: Vec<Run>, tail: &Tail) -> Element {
        let (shown, hidden) = cut_hidden(runs);
        let mut spans = self.spans(shown, true);
        if hidden > 0 {
            spans.push(Span::plain("\u{a0}".repeat(hidden)));
        }
        let body = wrap_styled(at.first.clone(), at.cont.clone(), spans, Wrap::Word);
        if tail.nib.is_empty() {
            return body;
        }
        caret_at(body, tail.nib.clone())
    }

    /// h1 brand 粗体，h2 粗体，h3 及以下 fg2 粗体；标题下没有横线。
    fn heading_style(&self, level: u8) -> Style {
        match level {
            1 => Style::new(self.theme.brand, Color::Default, attr::BOLD),
            2 => Style::BOLD,
            _ => self.theme.fg2.with_attr(attr::BOLD),
        }
    }

    /// 引用里：line 色的细栏，文字 fg2。
    fn quoted(&self, at: &At) -> At {
        At {
            first: joined(&at.first, self.quote_rail()),
            cont: joined(&at.cont, self.quote_rail()),
            depth: at.depth,
            ink: self.theme.fg2,
        }
    }

    /* ---- 代码块 ---- */

    /// 一个代码块；正在写（`open`）时笔尖骑在代码末尾，还没有下边（§4.2「流式节奏」的「代码块」）。
    fn render_code(
        &self,
        lang: Option<&str>,
        code: &str,
        at: &At,
        nib: &[Span],
        open: bool,
    ) -> Element {
        let mut lines = self.highlighted(lang, code);
        if let Some(last) = lines.last_mut() {
            last.extend_from_slice(nib);
        }
        match self.theme.panel {
            // 内容缩进处的面板；语言在它第一行的右端。
            Some(panel) => {
                let label = lang
                    .map(|l| vec![Span::new(l.to_owned(), self.theme.fg3)])
                    .unwrap_or_default();
                code_panel(at.first.clone(), lines, panel, label, open)
            }
            None => {
                // 第一个铺出的行从 `at.first` 开始，其余接续。有语言标签时它占第一行，代码往下推。
                let body_first = if lang.is_some() { &at.cont } else { &at.first };
                let tag = lang.map(|l| {
                    single_line(
                        joined(&at.first, [Span::new(l.to_owned(), self.theme.fg3)]),
                        false,
                    )
                });
                let body = lines.into_iter().enumerate().map(|(i, l)| {
                    let first = if i == 0 { body_first } else { &at.cont };
                    // 代码是原样的：按字符折（从不在词之间断开、不合并空格）。
                    wrap_styled(
                        joined(first, self.code_rail()),
                        joined(&at.cont, self.code_rail()),
                        l,
                        Wrap::Char,
                    )
                });
                stack(tag.into_iter().chain(body))
            }
        }
    }

    /// 代码的各行：rich 主题且 [`syntax`] 认得这门语言时用安静的语法墨；否则原样。
    fn highlighted(&self, lang: Option<&str>, code: &str) -> Vec<Vec<Span>> {
        let lexed = lang
            .filter(|_| self.theme.rich)
            .and_then(|l| syntax::lex(l, code));
        match lexed {
            Some(lines) => lines
                .into_iter()
                .map(|toks| {
                    toks.into_iter()
                        .map(|t| Span::new(t.text.to_owned(), self.own(self.syntax_style(t.kind))))
                        .collect()
                })
                .collect(),
            None => code
                .split('\n')
                .map(|l| vec![Span::new(l.to_owned(), self.own(Style::DEFAULT))])
                .collect(),
        }
    }

    /// 每类记号用主题的语法墨；注释 fg3 斜体，其余是正文。
    fn syntax_style(&self, k: Kind) -> Style {
        let y = &self.theme.syntax;
        match k {
            Kind::Keyword => Style::fg(y.keyword),
            Kind::Str => Style::fg(y.string),
            Kind::Num => Style::fg(y.number),
            Kind::Func => Style::fg(y.function),
            Kind::Type => Style::fg(y.ty),
            Kind::Const => Style::fg(y.constant),
            Kind::Key => Style::fg(y.key),
            Kind::Comment => self.theme.fg3.with_attr(attr::ITALIC),
            Kind::Plain => Style::DEFAULT,
        }
    }

    /* ---- 列表 ---- */

    /// 有 `open` 时，最后一条的最后一块是流式的尾巴（[`Md::render_open`]）。
    fn render_list_block(&self, b: &Block, at: &At, open: Option<&Tail>) -> Element {
        let Block::List {
            ordered,
            start,
            tight,
            items,
        } = b
        else {
            return empty();
        };
        // 有序列表的序号按最宽的那个右对齐。
        let digits = if *ordered {
            (u64::from(*start) + items.len() as u64 - 1)
                .to_string()
                .len()
        } else {
            0
        };
        let item_elems = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let (marker, ink) = self.list_marker(*ordered, *start, idx, item, at, digits);
                let pad = " ".repeat(width_of(&marker));
                let inner = At {
                    first: joined(if idx == 0 { &at.first } else { &at.cont }, marker),
                    cont: joined(&at.cont, [Span::plain(pad)]),
                    depth: at.depth + 1,
                    ink,
                };
                let elems = match open {
                    Some(t) if idx == items.len() - 1 && !item.blocks.is_empty() => {
                        let n = item.blocks.len();
                        let mut elems = self.render_list(&item.blocks[..n - 1], &inner);
                        let last_at = if n == 1 { inner } else { inner.rest() };
                        elems.push(self.render_open(&item.blocks[n - 1], &last_at, t));
                        elems
                    }
                    _ => self.render_list(&item.blocks, &inner),
                };
                self.join_blocks(item.blocks.iter().zip(elems).collect(), !tight)
            })
            .collect();
        self.join_spaced(item_elems, !tight)
    }

    /// 一条的标记与文字的墨。任务的勾代替圆点：做完是 ok 色的 `✓`、文字 fg3，没做是 fg3 的 `□`。
    /// 圆点随深度变（`•` `◦` `▪`）；序号右对齐到 `digits`。
    fn list_marker(
        &self,
        ordered: bool,
        start: u32,
        idx: usize,
        item: &ListItem,
        at: &At,
        digits: usize,
    ) -> (Vec<Span>, Style) {
        match item.task {
            Some(true) => (
                vec![Span::new("✓", Style::fg(self.theme.ok)), space()],
                self.theme.fg3,
            ),
            Some(false) => (vec![Span::new("□", self.theme.fg3), space()], at.ink),
            None => {
                let mark = if ordered {
                    let n = format!("{}.", u64::from(start) + idx as u64);
                    format!("{}{n}", " ".repeat(digits + 1 - n.len()))
                } else {
                    BULLETS[at.depth % BULLETS.len()].to_owned()
                };
                (vec![Span::new(mark, self.theme.fg3), space()], at.ink)
            }
        }
    }

    /* ---- 表格 ---- */

    /// 造一个与宽度无关的 [`Element::Table`]；分列宽与格子折行在布局里做，那里知道宽度。
    fn render_table(&self, b: &Block, at: &At) -> Element {
        let Block::Table {
            align,
            header,
            rows,
        } = b
        else {
            return empty();
        };
        if align.is_empty() {
            return empty();
        }
        // 表格整张出现、整张落定一次，不算进最新的字（§4.2「笔」的「表格」）。
        let cell = |c: &[Inline], st: Style| self.spans(self.inline_runs(c, st), false);
        let header = header.iter().map(|c| cell(c, Style::BOLD)).collect();
        let rows = rows
            .iter()
            .map(|r| r.iter().map(|c| cell(c, Style::DEFAULT)).collect())
            .collect();
        table(
            at.first.clone(),
            at.cont.clone(),
            align.iter().map(|&a| column_align(a)).collect(),
            header,
            rows,
            self.theme.line,
        )
    }
}

fn column_align(a: Align) -> ColumnAlign {
    match a {
        Align::Right => ColumnAlign::Right,
        Align::Center => ColumnAlign::Center,
        Align::None | Align::Left => ColumnAlign::Left,
    }
}
