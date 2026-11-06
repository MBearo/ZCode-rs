// 复制自 apps/zcode-host/crates/tui/src/ui/selection.rs（feat/rs 79b6691c55）；F25 改了词的字符类、
// 软折行的拼接与选区的底色（docs/specs/rust-cli/tui-select.md §2）。
//! 鼠标选区的文字处理（docs/specs/rust-tui.md §8.7）：按显示列取文字、找词与网址、换底色、选区范围。
//!
//! 行是 kit 的 [`StyledLine`]（tui-kit.md §3.1、§3.8）：改样式经 `StyledLine::restyle_cells`，宽度只从 kit 的
//! `render::width` 取；软折行的续行看 `cont`。
use rust_cli_tui_kit::render::width::display_width;
use rust_cli_tui_kit::render::{Continuation, StyledLine};

use super::ink::Ink;

fn width(c: char) -> usize {
    display_width(c)
}

/// `text` 里按显示列取 `[from, to)`。
pub fn columns(text: &str, from: usize, to: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = width(c);
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
            *used += width(c);
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
    let end_col = cells
        .get(end)
        .map_or_else(|| text.chars().map(width).sum(), |(at, _)| *at);
    (cells[start].0, end_col)
}

/// 显示列 `col` 处是不是文字：不是空白，也没有超出行尾；宽字符的两列都算。点击与悬停展开块时用（§8.2）。
pub fn text_at(text: &str, col: usize) -> bool {
    let mut used = 0;
    for c in text.chars() {
        let w = width(c);
        if (used..used + w).contains(&col) {
            return !c.is_whitespace();
        }
        used += w;
    }
    false
}

/// 把行的 `[from, to)` 列标成鼠标选区（tui-select.md §2.2）：rich 时只换底色、前景不变；plain 反显（`Ink::mouse_selection`，
/// Puffin `view/frame.rs` 的 `highlight`）。
pub fn mark_line(line: &mut StyledLine, from: usize, to: usize, ink: &Ink) {
    *line = line.restyle_cells(from, to, |style| ink.mouse_selection(style));
}

/// 选区里的一行：显示的文字，与它是不是上一行的软折行续行。
/// `soft` 是 kit 行的 `cont`（续行文字开始的列、折行处吃没吃掉空格）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub soft: Option<Continuation>,
}

/// 选区的文字（tui-select.md §2.3）：第一行从 `first` 列起，最后一行到 `last` 列（不含）；软折行的
/// 续行接到上一行后面（缩进不取，折行处吃掉的空格补回一个），后面接着续行的行不去行尾空白，其余行去掉；行之间用换行。
///
/// 缺陷修复（F25）：原来每一屏行都用换行连接、续行的缩进也取，折过行的错误复制出来是断开的几段（长的请求 ID
/// 被硬断时中间多了换行与空格）。
///
/// 照 Puffin 的复制（docs/puffin-tui.md §4.2「选择与复制」，tui-kit.md §4.2，S1）：只有半块字符的行（板的 `▄` `▀` 边）
/// 不复制；各行共同的前导缩进去掉（正文从第 3 列起，复制出来从行首起）。
pub fn join(rows: &[Row], first: usize, last: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let edge = row.text.trim();
        if !edge.is_empty() && edge.chars().all(|c| c == '▄' || c == '▀') {
            continue;
        }
        let soft = row.soft.filter(|_| index > 0);
        let mut from = if index == 0 { first } else { 0 };
        let to = if index + 1 == rows.len() {
            last
        } else {
            usize::MAX
        };
        let mut piece = String::new();
        if let Some(soft) = soft {
            if soft.space && from <= soft.at && to >= soft.at {
                piece.push(' ');
            }
            from = from.max(soft.at);
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
    let indent = out
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    out.iter()
        .map(|line| line.get(indent..).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
