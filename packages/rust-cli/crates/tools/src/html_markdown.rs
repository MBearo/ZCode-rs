//! HTML → Markdown（WebFetch，docs/specs/rust-cli/tools-more.md §4）。
//!
//! 近似 turndown 的默认配置（去掉 style / script / noscript / iframe；h1、h2 用 setext，
//! 其余 atx；列表标记 `*`；代码块缩进 4 格；强调 `_`，加粗 `**`；链接内联）。纯字符串处理，不解析
//! 完整的 DOM：未闭合与错误嵌套的标签按出现顺序处理。不做 turndown 的 Markdown 字符转义（§7 D9）。

/// 内容被整段去掉的元素。
const REMOVED: &[&str] = &["style", "script", "noscript", "iframe", "head", "template"];
/// 前后各空一行的块级元素。
const BLOCKS: &[&str] = &[
    "p", "div", "section", "article", "main", "header", "footer", "nav", "aside", "table", "form",
    "figure", "dl", "address", "fieldset",
];
/// 只换行的元素。
const LINES: &[&str] = &["tr", "dt", "dd", "figcaption", "caption"];

struct Tag<'a> {
    name: String,
    closing: bool,
    raw: &'a str,
}

fn parse_tag(raw: &str) -> Tag<'_> {
    let inner = raw.trim_start_matches('<').trim_end_matches('>');
    let closing = inner.starts_with('/');
    let name = inner
        .trim_start_matches('/')
        .split(|ch: char| ch.is_whitespace() || ch == '/' || ch == '>')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    Tag { name, closing, raw }
}

/// 属性值（`name="v"`、`name='v'`、`name=v`）。
fn attribute(raw: &str, name: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = raw[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or_default(),
            _ => rest
                .split(|ch: char| ch.is_whitespace() || ch == '>')
                .next()
                .unwrap_or_default(),
        };
        return Some(decode_entities(value));
    }
    None
}

/// 常见实体与数字实体。
pub(crate) fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Default)]
struct Writer {
    out: String,
    /// 打开中的引用块在 `out` 里的起点；关闭时给这段每行加 `> `。
    quotes: Vec<usize>,
    /// `<pre>` 里：保留空白，行首缩进 4 格。
    pre: usize,
    /// 列表栈：有序时记下一个序号。
    lists: Vec<Option<u32>>,
    /// 当前链接的目标（遇到 `</a>` 时写出）。
    links: Vec<Option<String>>,
}

impl Writer {
    fn blank_lines(&mut self, count: usize) {
        let trimmed = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(trimmed);
        if self.out.is_empty() {
            return;
        }
        let have = self.out.chars().rev().take_while(|ch| *ch == '\n').count();
        for _ in have..count {
            self.out.push('\n');
        }
    }

    fn text(&mut self, text: &str) {
        let text = decode_entities(text);
        if self.pre > 0 {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    self.out.push('\n');
                }
                if self.out.ends_with('\n') || index > 0 {
                    self.out.push_str("    ");
                }
                self.out.push_str(line);
            }
            return;
        }
        let mut collapsed = String::new();
        for (index, word) in text.split_whitespace().enumerate() {
            if index > 0 {
                collapsed.push(' ');
            }
            collapsed.push_str(word);
        }
        let leading = text.starts_with(char::is_whitespace);
        let trailing = text.ends_with(char::is_whitespace) && !collapsed.is_empty();
        if leading && !self.out.is_empty() && !self.out.ends_with([' ', '\n']) {
            self.out.push(' ');
        }
        self.out.push_str(&collapsed);
        if trailing {
            self.out.push(' ');
        }
    }

    fn open(&mut self, tag: &Tag<'_>) {
        let name = tag.name.as_str();
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.blank_lines(2);
                let level = usize::from(name.as_bytes()[1] - b'0');
                if level > 2 {
                    self.out.push_str(&"#".repeat(level));
                    self.out.push(' ');
                }
            }
            "br" => self.out.push_str("  \n"),
            "hr" => {
                self.blank_lines(2);
                self.out.push_str("* * *");
                self.blank_lines(2);
            }
            "pre" => {
                self.blank_lines(2);
                self.pre += 1;
                self.out.push_str("    ");
            }
            "code" if self.pre == 0 => self.out.push('`'),
            "strong" | "b" => self.out.push_str("**"),
            "em" | "i" => self.out.push('_'),
            "blockquote" => {
                self.blank_lines(2);
                self.quotes.push(self.out.len());
            }
            "ul" | "ol" => {
                self.blank_lines(if self.lists.is_empty() { 2 } else { 1 });
                self.lists.push((name == "ol").then_some(1));
            }
            "li" => {
                self.blank_lines(1);
                let depth = self.lists.len().saturating_sub(1);
                self.out.push_str(&"    ".repeat(depth));
                match self.lists.last_mut() {
                    Some(Some(number)) => {
                        self.out.push_str(&format!("{number}.  "));
                        *number += 1;
                    }
                    _ => self.out.push_str("*   "),
                }
            }
            "a" => {
                let href = attribute(tag.raw, "href");
                if href.is_some() {
                    self.out.push('[');
                }
                self.links.push(href);
            }
            "img" => {
                if let Some(src) = attribute(tag.raw, "src") {
                    let alt = attribute(tag.raw, "alt").unwrap_or_default();
                    self.out.push_str(&format!("![{alt}]({src})"));
                }
            }
            _ if BLOCKS.contains(&name) => self.blank_lines(2),
            _ if LINES.contains(&name) => self.blank_lines(1),
            "td" | "th" => self.out.push(' '),
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        match name {
            "h1" | "h2" => {
                let title = self.out.rsplit('\n').next().unwrap_or_default();
                let underline = if name == "h1" { '=' } else { '-' };
                let width = title.chars().count().max(1);
                self.out.push('\n');
                self.out.push_str(&underline.to_string().repeat(width));
                self.blank_lines(2);
            }
            "h3" | "h4" | "h5" | "h6" => self.blank_lines(2),
            "pre" => {
                self.pre = self.pre.saturating_sub(1);
                self.blank_lines(2);
            }
            "code" if self.pre == 0 => self.out.push('`'),
            "strong" | "b" => self.out.push_str("**"),
            "em" | "i" => self.out.push('_'),
            "blockquote" => {
                let start = self
                    .quotes
                    .pop()
                    .unwrap_or(self.out.len())
                    .min(self.out.len());
                let inner = self.out[start..].trim().to_owned();
                self.out.truncate(start);
                let quoted: Vec<String> = inner
                    .lines()
                    .map(|line| {
                        if line.is_empty() {
                            ">".to_owned()
                        } else {
                            format!("> {line}")
                        }
                    })
                    .collect();
                self.out.push_str(&quoted.join("\n"));
                self.blank_lines(2);
            }
            "ul" | "ol" => {
                self.lists.pop();
                self.blank_lines(if self.lists.is_empty() { 2 } else { 1 });
            }
            "a" => {
                if let Some(Some(href)) = self.links.pop() {
                    self.out.push_str(&format!("]({href})"));
                }
            }
            _ if BLOCKS.contains(&name) => self.blank_lines(2),
            _ if LINES.contains(&name) => self.blank_lines(1),
            _ => {}
        }
    }
}

/// 把 HTML 转成 Markdown。
pub(crate) fn to_markdown(html: &str) -> String {
    let mut writer = Writer::default();
    let mut rest = html;
    let mut skipping: Option<String> = None;
    while !rest.is_empty() {
        let Some(at) = rest.find('<') else {
            if skipping.is_none() {
                writer.text(rest);
            }
            break;
        };
        if at > 0 && skipping.is_none() {
            writer.text(&rest[..at]);
        }
        rest = &rest[at..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
            continue;
        }
        let end = rest.find('>').map_or(rest.len(), |end| end + 1);
        let tag = parse_tag(&rest[..end]);
        rest = &rest[end..];
        if tag.name.is_empty() || tag.name.starts_with('!') || tag.name.starts_with('?') {
            continue;
        }
        if let Some(skipped) = &skipping {
            if tag.closing && tag.name == *skipped {
                skipping = None;
            }
            continue;
        }
        if REMOVED.contains(&tag.name.as_str()) && !tag.closing {
            if !tag.raw.ends_with("/>") {
                skipping = Some(tag.name.clone());
            }
            continue;
        }
        if tag.closing {
            writer.close(&tag.name);
        } else {
            writer.open(&tag);
        }
    }
    writer
        .out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[cfg(test)]
#[path = "html_markdown_tests.rs"]
mod tests;
