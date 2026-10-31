// 复制自 apps/zcode-host/crates/tui/src/ui/markdown.rs（feat/rs 79b6691c55）。
//! markdown → 终端行（docs/specs/rust-tui.md §8.2）。标题粗体（一级加下划线、三级起加斜体），
//! 行内代码与代码块按主题着色，链接下划线，引用前缀 `> `，列表 `• ` / `1. `，任务 ☐ / ☑，
//! 嵌套列表缩进 4 列。缩进由前缀栈管理：每层有首行前缀与续行前缀。
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::highlight;
use super::palette::Palette;
use super::text::wrap;

#[derive(Clone)]
struct Level {
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    /// 这一层已经输出过一行：之后都用续行前缀。
    used: bool,
}

struct Renderer<'a> {
    palette: &'a Palette,
    width: usize,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    levels: Vec<Level>,
    /// 每层列表的下一个序号；无序列表为 None。
    lists: Vec<Option<u64>>,
    code: Option<(String, String)>,
    link: Option<String>,
    /// 下一个块之前要空一行。
    gap: bool,
    table: Option<Vec<Vec<String>>>,
    cell: String,
}

fn styled(text: &str, style: Style) -> Span<'static> {
    Span::styled(text.to_owned(), style)
}

impl<'a> Renderer<'a> {
    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, f: impl Fn(Style) -> Style) {
        let next = f(self.style());
        self.styles.push(next);
    }

    /// 当前的（首行，续行）前缀；取用后把各层标记为已使用。
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut first = Vec::new();
        let mut rest = Vec::new();
        for level in &self.levels {
            first.extend(if level.used {
                level.rest.clone()
            } else {
                level.first.clone()
            });
            rest.extend(level.rest.clone());
        }
        for level in &mut self.levels {
            level.used = true;
        }
        (first, rest)
    }

    fn blank(&mut self) {
        if self.gap && !self.lines.is_empty() {
            let (_, rest) = self.prefixes_peek();
            self.lines.push(Line::from(rest));
        }
        self.gap = false;
    }

    fn prefixes_peek(&self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let rest: Vec<Span<'static>> = self.levels.iter().flat_map(|l| l.rest.clone()).collect();
        (rest.clone(), rest)
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        self.blank();
        let spans = std::mem::take(&mut self.spans);
        let (first, rest) = self.prefixes();
        self.lines.extend(wrap(&spans, self.width, &first, &rest));
    }

    fn text(&mut self, text: &str) {
        if let Some((_, buffer)) = self.code.as_mut() {
            buffer.push_str(text);
            return;
        }
        if self.table.is_some() {
            self.cell.push_str(text);
            return;
        }
        let style = self.style();
        self.spans.push(styled(text, style));
    }

    fn code_block(&mut self, language: &str, code: &str) {
        self.blank();
        let code = code.strip_suffix('\n').unwrap_or(code);
        let highlighted = highlight::highlight(code, language, self.palette);
        let fallback = Style::default().fg(self.palette.accent);
        for (index, raw) in code.split('\n').enumerate() {
            let spans = highlighted
                .as_ref()
                .and_then(|lines| lines.get(index).cloned())
                .unwrap_or_else(|| vec![styled(raw, fallback)]);
            let (first, rest) = self.prefixes();
            self.lines.extend(wrap(&spans, self.width, &first, &rest));
        }
        self.gap = true;
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => {
                if self.table.is_some() {
                    self.cell.push_str(&code);
                } else {
                    let style = self.style().fg(self.palette.accent);
                    self.spans.push(styled(&code, style));
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.blank();
                let (first, _) = self.prefixes();
                /// 分隔线最长 40 列。
                const MAX_RULE_WIDTH: usize = 40;
                let room = self
                    .width
                    .saturating_sub(super::text::width(&first))
                    .min(MAX_RULE_WIDTH);
                let mut line = first;
                line.push(styled(
                    &"─".repeat(room),
                    Style::default().add_modifier(Modifier::DIM),
                ));
                self.lines.push(Line::from(line));
                self.gap = true;
            }
            Event::TaskListMarker(done) => {
                // 任务列表：把列表项的标记换成方框。
                if let Some(level) = self.levels.last_mut() {
                    let lead = level.rest.clone();
                    let mut first: Vec<Span<'static>> =
                        lead[..lead.len().saturating_sub(1)].to_vec();
                    first.push(Span::raw(if done { "☑ " } else { "☐ " }));
                    level.first = first;
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::InlineMath(math) | Event::DisplayMath(math) => self.text(&math),
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                self.push_style(|s| {
                    let s = s.add_modifier(Modifier::BOLD);
                    match level {
                        HeadingLevel::H1 => s.add_modifier(Modifier::UNDERLINED),
                        HeadingLevel::H2 => s,
                        _ => s.add_modifier(Modifier::ITALIC),
                    }
                });
            }
            Tag::BlockQuote(_) => {
                self.flush();
                let quote = Style::default().add_modifier(Modifier::DIM);
                self.levels.push(Level {
                    first: vec![styled("▎ ", quote)],
                    rest: vec![styled("▎ ", quote)],
                    used: false,
                });
                self.push_style(|s| s.add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                // 嵌套列表与上一层的正文之间不空行。
                if self.lists.is_empty() {
                    self.blank();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        styled(&marker, Style::default().fg(Color::LightBlue))
                    }
                    _ => Span::raw("• "),
                };
                // 嵌套层相对上一层缩进 4 列。
                let indent = if depth > 1 { "  " } else { "" };
                let pad = " ".repeat(marker.content.width());
                self.levels.push(Level {
                    first: vec![Span::raw(indent), marker],
                    rest: vec![Span::raw(indent), Span::raw(pad)],
                    used: false,
                });
            }
            Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                let accent = self.palette.accent;
                self.link = Some(dest_url.into_string());
                self.push_style(|s| s.fg(accent).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Vec::new());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(rows) = self.table.as_mut() {
                    rows.push(Vec::new());
                }
            }
            Tag::TableCell => self.cell.clear(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.gap = true;
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.styles.pop();
                self.gap = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.levels.pop();
                self.styles.pop();
                self.gap = true;
            }
            TagEnd::CodeBlock => {
                if let Some((language, code)) = self.code.take() {
                    self.code_block(&language, &code);
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.gap = self.lists.is_empty();
            }
            TagEnd::Item => {
                self.flush();
                self.levels.pop();
                // 列表项之间不空行（项内段落之间仍空一行）。
                self.gap = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some(url) = self.link.take() {
                    let shown: String = self
                        .spans
                        .iter()
                        .rev()
                        .take(1)
                        .map(|s| s.content.to_string())
                        .collect();
                    // 文字就是网址、或是工作区里的文件链接时不再重复显示网址。
                    if url.starts_with("http") && shown != url {
                        self.spans.push(styled(
                            &format!(" ({url})"),
                            Style::default().add_modifier(Modifier::DIM),
                        ));
                    }
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.cell);
                if let Some(row) = self.table.as_mut().and_then(|rows| rows.last_mut()) {
                    row.push(cell.trim().to_owned());
                }
            }
            TagEnd::Table => {
                if let Some(rows) = self.table.take() {
                    self.table(rows);
                }
            }
            _ => {}
        }
    }
}

/// 排版状态里除已输出的行以外的部分：顶层块之间按它续排（[`memo`]）。
#[derive(Clone)]
struct State {
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    levels: Vec<Level>,
    lists: Vec<Option<u64>>,
    code: Option<(String, String)>,
    link: Option<String>,
    gap: bool,
    table: Option<Vec<Vec<String>>>,
    cell: String,
}

impl<'a> Renderer<'a> {
    fn new(
        palette: &'a Palette,
        width: usize,
        (first, rest): (Vec<Span<'static>>, Vec<Span<'static>>),
        base: Style,
    ) -> Self {
        Renderer {
            palette,
            width,
            lines: Vec::new(),
            spans: Vec::new(),
            styles: vec![base],
            levels: vec![Level {
                first,
                rest,
                used: false,
            }],
            lists: Vec::new(),
            code: None,
            link: None,
            gap: false,
            table: None,
            cell: String::new(),
        }
    }

    fn state(&self) -> State {
        State {
            spans: self.spans.clone(),
            styles: self.styles.clone(),
            levels: self.levels.clone(),
            lists: self.lists.clone(),
            code: self.code.clone(),
            link: self.link.clone(),
            gap: self.gap,
            table: self.table.clone(),
            cell: self.cell.clone(),
        }
    }

    fn restore(&mut self, state: &State) {
        let state = state.clone();
        self.spans = state.spans;
        self.styles = state.styles;
        self.levels = state.levels;
        self.lists = state.lists;
        self.code = state.code;
        self.link = state.link;
        self.gap = state.gap;
        self.table = state.table;
        self.cell = state.cell;
    }

    /// 全部事件之后：流式中未闭合的代码块也显示出来。
    fn finish(mut self) -> Vec<Line<'static>> {
        if let Some((language, code)) = self.code.take() {
            self.code_block(&language, &code);
        }
        self.flush();
        self.lines
    }
}

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS);

/// 渲染 markdown。`first` / `rest` 是 cell 自己的首行与续行前缀（如 `• ` 与两个空格）。
pub fn render(
    text: &str,
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    base: Style,
    palette: &Palette,
) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(palette, width, (first, rest), base);
    for event in Parser::new_ext(text, OPTIONS) {
        renderer.event(event);
    }
    renderer.finish()
}

#[path = "markdown_memo.rs"]
mod memo;
pub use memo::Memo;

#[path = "markdown_table.rs"]
mod table;

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
