//! 折行与截断，kit 的类型（docs/specs/rust-cli/tui-kit.md §3.1、§3.6、§3.8，工作线 TK 的 C0）：`text.rs` 的同一套规则，
//! 输入输出换成 kit 的 [`Span`] / [`StyledLine`]，宽度只从 kit 的 `render::width` 取（与渲染器、`Surface` 同一张表）。
//! 过渡期与 `text.rs` 并存；C2 删掉 `text.rs`。
//!
//! 规则（照旧，docs/specs/rust-tui.md §8.2）：按显示宽度计算，CJK 占两列；优先在空白处断开，单词超过一行时硬断；宽字符
//! 之间可以断开。首行与续行可以有不同的前缀（`• ` 与两个空格、`└ ` 与四个空格）。续行带
//! `cont: Some(Continuation { at, space })`：`at` 是续行前缀的宽度（复制时不取），`space` 是折行处吃掉了一个空格（复制时
//! 补回），与 `text::SoftWrap` 的记号逐项对应（tui-select.md §2.3）。
use rust_cli_tui_kit::render::width::{display_width, string_width};
use rust_cli_tui_kit::render::{Continuation, Span, Style, StyledLine};

/// 片段的显示宽度之和。
pub fn width(spans: &[Span]) -> usize {
    spans.iter().map(|span| string_width(&span.text)).sum()
}

/// 把带样式的片段折成多行。`first` 与 `rest` 是首行与续行的前缀；`width` 是含前缀的总宽度。续行带
/// [`Continuation`]，复制时据此把续行接回上一行（F25）。
pub fn wrap(spans: &[Span], width: usize, first: &[Span], rest: &[Span]) -> Vec<StyledLine> {
    let mut lines: Vec<StyledLine> = vec![StyledLine::new(first.to_vec())];
    let mut room = width.saturating_sub(self::width(first)).max(1);
    let indent = self::width(rest);
    let rest_room = width.saturating_sub(indent).max(1);
    let mut used = 0usize;
    // 当前行末尾吃掉了一个空白（折行处的空格）。
    let mut dropped = false;
    let newline =
        |lines: &mut Vec<StyledLine>, used: &mut usize, room: &mut usize, dropped: &mut bool| {
            lines.push(StyledLine {
                spans: rest.to_vec(),
                cont: Some(Continuation {
                    at: indent,
                    space: std::mem::take(dropped),
                }),
            });
            *used = 0;
            *room = rest_room;
        };
    let push = |lines: &mut Vec<StyledLine>, text: String, span: &Span| {
        if let Some(line) = lines.last_mut() {
            line.spans
                .push(Span::with_decor(text, span.style, span.decor.clone()));
        }
    };
    for span in spans {
        // 词：连续的非空白字符；空白与宽字符各自成词（宽字符之间可以断开）。
        let mut words: Vec<String> = Vec::new();
        let mut current = String::new();
        for c in span.text.chars() {
            if c.is_whitespace() || display_width(c) > 1 {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push(c.to_string());
            } else {
                current.push(c);
            }
        }
        if !current.is_empty() {
            words.push(current);
        }
        for word in words {
            let word_width = string_width(&word);
            if used + word_width > room && used > 0 {
                if word.trim().is_empty() {
                    // 行尾的空白不带到下一行。
                    dropped = true;
                    continue;
                }
                newline(&mut lines, &mut used, &mut room, &mut dropped);
            }
            if used + word_width <= room {
                push(&mut lines, word, span);
                used += word_width;
                continue;
            }
            // 比一整行还宽：按字符硬断。
            let mut piece = String::new();
            for c in word.chars() {
                let w = display_width(c);
                if used + w > room && used > 0 {
                    push(&mut lines, std::mem::take(&mut piece), span);
                    newline(&mut lines, &mut used, &mut room, &mut dropped);
                }
                piece.push(c);
                used += w;
            }
            push(&mut lines, piece, span);
        }
    }
    lines
}

/// 纯文本按行折：保留原有换行，每段各自折行；首段用 `first` 前缀，其余段与续行用 `rest`。
pub fn wrap_text(
    text: &str,
    style: Style,
    width: usize,
    first: &[Span],
    rest: &[Span],
) -> Vec<StyledLine> {
    let mut out = Vec::new();
    for (index, part) in text.split('\n').enumerate() {
        let prefix = if index == 0 { first } else { rest };
        out.extend(wrap(
            &[Span::new(part.to_owned(), style)],
            width,
            prefix,
            rest,
        ));
    }
    out
}

/// 按显示宽度截断，超出时以 `…` 结尾。
pub fn truncate(text: &str, width: usize) -> String {
    if string_width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = display_width(c);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// 把一行截到 `width` 列（逐个片段截，样式与 `cont` 保留）。
pub fn truncate_line(line: StyledLine, width: usize) -> StyledLine {
    if line.width() <= width {
        return line;
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for span in line.spans {
        let w = string_width(&span.text);
        if used + w < width {
            used += w;
            spans.push(span);
            continue;
        }
        let cut = truncate(&span.text, width - used);
        spans.push(Span::with_decor(cut, span.style, span.decor));
        break;
    }
    StyledLine {
        spans,
        cont: line.cont,
    }
}

/// 行的纯文本（测试比对用）。
#[cfg(test)]
pub fn plain(line: &StyledLine) -> String {
    line.plain()
}

#[cfg(test)]
#[path = "lines_tests.rs"]
mod tests;
