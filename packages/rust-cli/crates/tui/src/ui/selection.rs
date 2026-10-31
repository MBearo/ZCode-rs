// 复制自 apps/zcode-host/crates/tui/src/ui/selection.rs（feat/rs 79b6691c55）；F25 改了词的字符类、
// 软折行的拼接与选区的底色（docs/specs/rust-cli/tui-select.md §2）。
//! 鼠标选区的文字处理（docs/specs/rust-tui.md §8.7）：按显示列取文字、找词与网址、换底色、选区范围。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::mouse::{Point, Selection};

/// `text` 里按显示列取 `[from, to)`。
pub fn columns(text: &str, from: usize, to: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used >= from && used < to {
            out.push(c);
        }
        used += w;
    }
    out
}

/// 词的字符类：空白；字母、数字与 `_/.-+~\\`；其他。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Other,
}

fn class(c: char) -> Class {
    if c == ' ' {
        Class::Space
    } else if c.is_alphanumeric() || "_/.-+~\\".contains(c) {
        Class::Word
    } else {
        Class::Other
    }
}

/// 显示列 `col` 处的词：与它同一字符类的连续字符。返回列范围（不含结束列）。
pub fn word_at(text: &str, col: usize) -> (usize, usize) {
    let cells: Vec<(usize, char)> = text
        .chars()
        .scan(0, |used, c| {
            let at = *used;
            *used += c.width().unwrap_or(0);
            Some((at, c))
        })
        .collect();
    let Some(index) = cells.iter().rposition(|(at, _)| *at <= col) else {
        return (col, col + 1);
    };
    let kind = class(cells[index].1);
    let word = |c: char| class(c) == kind;
    let start = cells[..=index]
        .iter()
        .rposition(|(_, c)| !word(*c))
        .map_or(0, |i| i + 1);
    let end = cells[index..]
        .iter()
        .position(|(_, c)| !word(*c))
        .map_or(cells.len(), |i| index + i);
    let end_col = cells.get(end).map_or_else(
        || text.chars().map(|c| c.width().unwrap_or(0)).sum(),
        |(at, _)| *at,
    );
    (cells[start].0, end_col)
}

/// 显示列 `col` 处是不是文字：不是空白，也没有超出行尾；宽字符的两列都算。点击与悬停展开块时用（§8.2）。
pub fn text_at(text: &str, col: usize) -> bool {
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if (used..used + w).contains(&col) {
            return !c.is_whitespace();
        }
        used += w;
    }
    false
}

/// 行里 `col` 处的网址（`http://` 或 `https://` 开头）。
pub fn url_at(text: &str, col: usize) -> Option<String> {
    let mut used = 0;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        if c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '"' | '\'') {
            if used > col {
                break;
            }
            start = index + c.len_utf8();
        }
        used += c.width().unwrap_or(0);
    }
    let token: String = text[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '<' | '>' | '"' | '\''))
        .collect();
    let token = token.trim_end_matches(['.', ',', ';', ':']);
    (token.starts_with("http://") || token.starts_with("https://")).then(|| token.to_owned())
}

/// 把行的 `[from, to)` 列反色（按需拆开片段）。输入框的选区用。
pub fn reverse(line: &mut Line<'static>, from: usize, to: usize) {
    restyle(line, from, to, |style| {
        style.add_modifier(Modifier::REVERSED)
    });
}

/// 把行的 `[from, to)` 列叠上 `style`（搜索的高亮，tui-input.md §2.3.5）：`style` 里设了的颜色与修饰键盖过原来的。
pub fn paint(line: &mut Line<'static>, from: usize, to: usize, style: Style) {
    restyle(line, from, to, |base| base.patch(style));
}

/// 把行的 `[from, to)` 列换成选区底色（只换底色，前景不变，tui-select.md §2.2）。
pub fn highlight(line: &mut Line<'static>, from: usize, to: usize, bg: ratatui::style::Color) {
    restyle(line, from, to, |style| style.bg(bg));
}

/// 把行的 `[from, to)` 列的样式按 `change` 改（按需拆开片段）。零宽的片段（软折行的记号）原样保留。
fn restyle(line: &mut Line<'static>, from: usize, to: usize, change: impl Fn(Style) -> Style) {
    let mut spans = Vec::new();
    let mut used = 0;
    for span in std::mem::take(&mut line.spans) {
        if span.content.is_empty() {
            spans.push(span);
            continue;
        }
        let mut current = String::new();
        let mut current_selected = None;
        for c in span.content.chars() {
            let selected = used >= from && used < to;
            if current_selected.is_some_and(|s| s != selected) {
                let style = if current_selected == Some(true) {
                    change(span.style)
                } else {
                    span.style
                };
                spans.push(Span::styled(std::mem::take(&mut current), style));
            }
            current_selected = Some(selected);
            current.push(c);
            used += c.width().unwrap_or(0);
        }
        if !current.is_empty() {
            let style = if current_selected == Some(true) {
                change(span.style)
            } else {
                span.style
            };
            spans.push(Span::styled(current, style));
        }
    }
    line.spans = spans;
}

/// 选区在全部行里的范围：（起始行，起始列，结束行，结束列（不含））；端点找不到时为 None。`find` 按
/// （块的键，块内行）给出行号。
pub fn range(
    selection: &Selection,
    find: impl Fn(&Point) -> Option<usize>,
) -> Option<(usize, usize, usize, usize)> {
    let a = (find(&selection.anchor)?, selection.anchor.col);
    let b = (find(&selection.head)?, selection.head.col);
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    Some((start.0, start.1, end.0, end.1 + 1))
}

/// 选区里的一行：显示的文字，与它是不是上一行的软折行续行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub soft: Option<super::text::SoftWrap>,
}

/// 选区的文字（tui-select.md §2.3）：第一行从 `first` 列起，最后一行到 `last` 列（不含）；软折行的
/// 续行接到上一行后面（缩进不取，折行处吃掉的空格补回一个），后面接着续行的行不去行尾空白，其余行去掉；行之间用换行。
///
/// 缺陷修复（F25）：原来每一屏行都用换行连接、续行的缩进也取，折过行的错误复制出来是断开的几段（长的请求 ID
/// 被硬断时中间多了换行与空格）。
pub fn join(rows: &[Row], first: usize, last: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let soft = row.soft.filter(|_| index > 0);
        let mut from = if index == 0 { first } else { 0 };
        let to = if index + 1 == rows.len() {
            last
        } else {
            usize::MAX
        };
        let mut piece = String::new();
        if let Some(soft) = soft {
            if soft.space && from <= soft.indent && to >= soft.indent {
                piece.push(' ');
            }
            from = from.max(soft.indent);
        }
        piece.push_str(&columns(&row.text, from, to));
        let continued = rows.get(index + 1).is_some_and(|next| next.soft.is_some());
        let piece = if continued {
            piece
        } else {
            piece.trim_end().to_owned()
        };
        match (soft, out.last_mut()) {
            (Some(_), Some(previous)) => previous.push_str(&piece),
            _ => out.push(piece),
        }
    }
    out.join("\n")
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
