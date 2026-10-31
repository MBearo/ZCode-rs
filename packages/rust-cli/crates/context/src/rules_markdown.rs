//! 规则文件正文的块扫描：去掉块级 HTML 注释、找 `@include`（context.md §11.2、§11.3）。
//!
//! 按 Markdown 切块（`gfm: false`），再在 html 块上去注释、在文本块上找 @include。这里只实现用得到的块：
//! 围栏代码、缩进代码、注释块与其余文本；列表与引用不单独建模（差异见 context.md §7）。

use std::sync::OnceLock;

use regex::Regex;

/// CommonMark：块的缩进最多 3 个空格（4 个起是缩进代码块）、围栏至少 3 个符号、标题 1–6 级。
const MAX_INDENT: usize = 3;
const MIN_FENCE: usize = 3;
const MAX_HEADING_LEVEL: usize = 6;
/// HTML 注释的开头与结尾。
const COMMENT_OPEN: &str = "<!--";
const COMMENT_CLOSE: &str = "-->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Code,
    Comment,
}

struct Block {
    kind: Kind,
    raw: String,
}

fn regex(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// 注释片段（非贪婪，可跨行）。
fn comment_span() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    regex(&CELL, r"(?s)<!--.*?-->")
}

fn remove_comments(text: &str) -> String {
    comment_span().map_or_else(
        || text.to_owned(),
        |span| span.replace_all(text, "").into_owned(),
    )
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// 围栏的开头：最多 3 个空格缩进，3 个以上的 ``` 或 ~~~。
fn fence(line: &str) -> Option<(char, usize)> {
    let body = line.trim_start_matches(' ');
    if indent(line) > MAX_INDENT {
        return None;
    }
    let mark = body.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let count = body.chars().take_while(|c| *c == mark).count();
    (count >= MIN_FENCE).then_some((mark, count))
}

fn closes(line: &str, (mark, count): (char, usize)) -> bool {
    let body = line.trim_start_matches(' ');
    indent(line) <= MAX_INDENT
        && body.chars().take_while(|c| *c == mark).count() >= count
        && body.trim_start_matches(mark).trim().is_empty()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_heading(line: &str) -> bool {
    let body = line.trim_start_matches(' ');
    let hashes = body.chars().take_while(|c| *c == '#').count();
    indent(line) <= MAX_INDENT
        && (1..=MAX_HEADING_LEVEL).contains(&hashes)
        && body[hashes..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

fn push(blocks: &mut Vec<Block>, kind: Kind, raw: &str) {
    match blocks.last_mut() {
        Some(last) if last.kind == kind && kind != Kind::Comment => last.raw.push_str(raw),
        _ => blocks.push(Block {
            kind,
            raw: raw.to_owned(),
        }),
    }
}

/// 切块。注释块从 `<!--` 到第一个 `-->` 所在行的行尾，再带上其后的空行（marked 的 html 块）；
/// 没有闭合时到文末。空行单独成块，段落之间的行内代码不会连起来。
fn blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut blocks = Vec::new();
    let mut paragraph = false;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if is_blank(line) {
            blocks.push(Block {
                kind: Kind::Text,
                raw: line.to_owned(),
            });
            paragraph = false;
            at += 1;
        } else if let Some(open) = fence(line) {
            let mut end = at + 1;
            while end < lines.len() && !closes(lines[end], open) {
                end += 1;
            }
            let end = (end + 1).min(lines.len());
            push(&mut blocks, Kind::Code, &lines[at..end].concat());
            paragraph = false;
            at = end;
        } else if !paragraph && (indent(line) > MAX_INDENT || line.starts_with('\t')) {
            push(&mut blocks, Kind::Code, line);
            at += 1;
        } else if indent(line) <= MAX_INDENT
            && line.trim_start_matches(' ').starts_with(COMMENT_OPEN)
        {
            let rest = lines[at..].concat();
            let open = rest.find(COMMENT_OPEN).unwrap_or_default() + COMMENT_OPEN.len();
            // marked：`<!-->`、`<!--->` 当场闭合，否则到第一个 `-->`。
            let after = &rest[open..];
            let close = if after.starts_with('>') {
                Some(open + 1)
            } else if after.starts_with("->") {
                Some(open + "->".len())
            } else {
                after
                    .find(COMMENT_CLOSE)
                    .map(|found| open + found + COMMENT_CLOSE.len())
            };
            let mut end = match close {
                Some(close) => at + rest[..close].matches('\n').count() + 1,
                None => lines.len(),
            };
            while close.is_some() && end < lines.len() && lines[end] == "\n" {
                end += 1;
            }
            let end = end.max(at + 1);
            blocks.push(Block {
                kind: Kind::Comment,
                raw: lines[at..end].concat(),
            });
            paragraph = false;
            at = end;
        } else {
            push(&mut blocks, Kind::Text, line);
            paragraph = !is_heading(line);
            at += 1;
        }
    }
    blocks
}

/// 注释块完整闭合（marked token 以 `<!--` 开头且含 `-->`）。
fn closed(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    trimmed.starts_with("<!--") && trimmed.contains("-->")
}

/// 去掉块级 HTML 注释：完整注释块去掉注释片段，
/// 余下的非空白文字保留；未闭合的注释原样保留。只在正文含 `<!--` 时调用（此时 `\r\n` 已换成 `\n`）。
pub(crate) fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for block in blocks(text) {
        if block.kind == Kind::Comment && closed(&block.raw) {
            let residue = remove_comments(&block.raw);
            if !residue.trim().is_empty() {
                out.push_str(&residue);
            }
        } else {
            out.push_str(&block.raw);
        }
    }
    out
}

/// 从 `from` 起，恰好 `count` 个反引号的下一段的位置。
fn closing_run(text: &str, from: usize, count: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let run = bytes[at..].iter().take_while(|byte| **byte == b'`').count();
            if run == count {
                return Some(at);
            }
            at += run;
        } else {
            at += 1;
        }
    }
    None
}

/// 行内的 HTML 标签（`<b>`、`</span>`、`<br/>`）。
fn tag() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    regex(&CELL, r"\A</?[A-Za-z][A-Za-z0-9-]*(?:\s[^<>]*)?/?>")
}

/// 文本按行内代码、HTML 注释与标签切成片段（marked 的 inline token 之间的 text）。行内代码以
/// 同样长度的反引号闭合，没有闭合的反引号是普通文字。
fn segments(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        let skip = match bytes[at] {
            b'`' => {
                let run = bytes[at..].iter().take_while(|byte| **byte == b'`').count();
                match closing_run(text, at + run, run) {
                    Some(close) => Some(close + run),
                    None => {
                        at += run;
                        continue;
                    }
                }
            }
            b'<' if text[at..].starts_with(COMMENT_OPEN) => {
                let body = at + COMMENT_OPEN.len();
                text[body..]
                    .find(COMMENT_CLOSE)
                    .map(|end| body + end + COMMENT_CLOSE.len())
            }
            b'<' => tag()
                .and_then(|tag| tag.find(&text[at..]))
                .map(|found| at + found.end()),
            _ => None,
        };
        match skip {
            Some(end) => {
                parts.push(&text[start..at]);
                start = end;
                at = end;
            }
            None => at += 1,
        }
    }
    parts.push(&text[start..]);
    parts
}

/// 接受的 @include 写法。
fn valid(path: &str) -> bool {
    path.starts_with("./")
        || path.starts_with("~/")
        || (path.starts_with('/') && path != "/")
        || (!path.starts_with('@')
            && !path.starts_with(['#', '%', '^', '&', '*', '(', ')'])
            && path
                .starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
}

fn scan(text: &str, found: &mut Vec<String>) {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    let Some(include) = regex(&CELL, r"(?:^|\s)@((?:[^\s\\]|\\ )+)") else {
        return;
    };
    for captures in include.captures_iter(text) {
        let Some(raw) = captures.get(1) else { continue };
        let path = raw.as_str();
        let path = path.find('#').map_or(path, |at| &path[..at]);
        let path = path.replace("\\ ", " ");
        if !path.is_empty() && valid(&path) && !found.contains(&path) {
            found.push(path);
        }
    }
}

/// @include 的原文路径（未解析），按出现顺序去重。代码块里的不算；注释块去掉注释后的余下文字照找。
pub(crate) fn includes(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for block in blocks(text) {
        match block.kind {
            Kind::Code => {}
            Kind::Comment if closed(&block.raw) => {
                let residue = remove_comments(&block.raw);
                if !residue.trim().is_empty() {
                    scan(&residue, &mut found);
                }
            }
            Kind::Comment => {}
            Kind::Text => {
                for segment in segments(&block.raw) {
                    scan(segment, &mut found);
                }
            }
        }
    }
    found
}

#[cfg(test)]
#[path = "rules_markdown_tests.rs"]
mod tests;
