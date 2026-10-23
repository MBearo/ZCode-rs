// 复制自 apps/zcode-host/crates/tui/src/ui/text.rs（feat/rs 79b6691c55）。
//! 折行与截断（docs/specs/rust-tui.md §8.2）：按显示宽度计算，CJK 占两列；优先在空白处断开，
//! 单词超过一行时硬断。首行与续行可以有不同的前缀（`• ` 与两个空格、`└ ` 与四个空格）。
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

/// 软折行的记号（屏幕的 `softWrap`，docs/specs/rust-cli/tui-select.md §2.3）：这一行是上一行排不下折过来
/// 的续行；`indent` 是续行前缀的宽度（复制时不取），`space` 是折行处吃掉了一个空格（复制时补回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftWrap {
    pub indent: usize,
    pub space: bool,
}

/// 记号片段的标记值：下划线颜色 `Rgb(MARK, 缩进, 吃掉空格)`。片段是空串，不画任何格子，也不占宽度。
const MARK: u8 = 0xF2;

fn soft_mark(soft: SoftWrap) -> Span<'static> {
    let indent = u8::try_from(soft.indent).unwrap_or(u8::MAX);
    Span::styled(
        "",
        Style::new().underline_color(ratatui::style::Color::Rgb(
            MARK,
            indent,
            u8::from(soft.space),
        )),
    )
}

/// 行上的软折行记号（[`wrap`] 写在续行上）。
pub fn soft_wrap(line: &Line<'_>) -> Option<SoftWrap> {
    line.spans.iter().find_map(
        |span| match (span.content.is_empty(), span.style.underline_color) {
            (true, Some(ratatui::style::Color::Rgb(MARK, indent, space))) => Some(SoftWrap {
                indent: usize::from(indent),
                space: space == 1,
            }),
            _ => None,
        },
    )
}

/// 把带样式的片段折成多行。`first` 与 `rest` 是首行与续行的前缀；`width` 是含前缀的总宽度。续行末尾放一个零宽的
/// 软折行记号（[`soft_wrap`]），复制时据此把续行接回上一行（F25）。
pub fn wrap(
    spans: &[Span<'_>],
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    let mut lines: Vec<Vec<Span<'static>>> = vec![first.to_vec()];
    let mut softs: Vec<Option<SoftWrap>> = vec![None];
    let mut room = width.saturating_sub(self::width(first)).max(1);
    let indent = self::width(rest);
    let rest_room = width.saturating_sub(indent).max(1);
    let mut used = 0usize;
    // 当前行末尾吃掉了一个空白（折行处的空格）。
    let mut dropped = false;
    let mut newline = |lines: &mut Vec<Vec<Span<'static>>>,
                       used: &mut usize,
                       room: &mut usize,
                       dropped: &mut bool| {
        lines.push(rest.to_vec());
        softs.push(Some(SoftWrap {
            indent,
            space: std::mem::take(dropped),
        }));
        *used = 0;
        *room = rest_room;
    };
    for span in spans {
        let style = span.style;
        // 词：连续的非空白字符；空白与宽字符各自成词（宽字符之间可以断开）。
        let mut words: Vec<String> = Vec::new();
        let mut current = String::new();
        for c in span.content.chars() {
            if c.is_whitespace() || c.width().unwrap_or(0) > 1 {
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
            let word_width = word.width();
            if used + word_width > room && used > 0 {
                if word.trim().is_empty() {
                    // 行尾的空白不带到下一行。
                    dropped = true;
                    continue;
                }
                newline(&mut lines, &mut used, &mut room, &mut dropped);
            }
            if used + word_width <= room {
                if let Some(line) = lines.last_mut() {
                    line.push(Span::styled(word, style));
                }
                used += word_width;
                continue;
            }
            // 比一整行还宽：按字符硬断。
            let mut piece = String::new();
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                if used + w > room && used > 0 {
                    if let Some(line) = lines.last_mut() {
                        line.push(Span::styled(std::mem::take(&mut piece), style));
                    }
                    newline(&mut lines, &mut used, &mut room, &mut dropped);
                }
                piece.push(c);
                used += w;
            }
            if let Some(line) = lines.last_mut() {
                line.push(Span::styled(piece, style));
            }
        }
    }
    lines
        .into_iter()
        .zip(softs)
        .map(|(mut spans, soft)| {
            if let Some(soft) = soft {
                spans.push(soft_mark(soft));
            }
            Line::from(spans)
        })
        .collect()
}

/// 纯文本按行折：保留原有换行，每段各自折行；首段用 `first` 前缀，其余段与续行用 `rest`。
pub fn wrap_text(
    text: &str,
    style: Style,
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (index, part) in text.split('\n').enumerate() {
        let prefix = if index == 0 { first } else { rest };
        out.extend(wrap(
            &[Span::styled(part.to_owned(), style)],
            width,
            prefix,
            rest,
        ));
    }
    out
}

/// 按显示宽度截断，超出时以 `…` 结尾。
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// 把一行截到 `width` 列（逐个 span 截，样式保留）。
pub fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let style = line.style;
    let mut spans = Vec::new();
    let mut used = 0;
    for span in line.spans {
        let w = span.content.width();
        if used + w < width {
            used += w;
            spans.push(span);
            continue;
        }
        let cut = truncate(&span.content, width - used);
        spans.push(Span::styled(cut, span.style));
        break;
    }
    Line::from(spans).style(style)
}

/// 行的纯文本（测试与复制用）。
pub fn plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(plain).collect()
    }

    #[test]
    fn words_wrap_under_a_hanging_prefix() {
        let lines = wrap(
            &[Span::raw("hello wide world")],
            9,
            &[Span::raw("• ")],
            &[Span::raw("  ")],
        );
        assert_eq!(texts(&lines), ["• hello ", "  wide ", "  world"]);
    }

    #[test]
    fn long_words_and_wide_characters_break_anywhere() {
        let lines = wrap(&[Span::raw("abcdefghij")], 4, &[], &[]);
        assert_eq!(texts(&lines), ["abcd", "efgh", "ij"]);
        let lines = wrap(
            &[Span::raw("你好世界")],
            6,
            &[Span::raw("› ")],
            &[Span::raw("  ")],
        );
        assert_eq!(texts(&lines), ["› 你好", "  世界"]);
    }

    #[test]
    fn styles_survive_and_text_keeps_its_line_breaks() {
        let bold = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
        let lines = wrap(&[Span::raw("a "), Span::styled("bold", bold)], 3, &[], &[]);
        assert_eq!(texts(&lines), ["a ", "bol", "d"]);
        assert_eq!(lines[1].spans[0].style, bold);
        let lines = wrap_text(
            "one\ntwo",
            Style::default(),
            10,
            &[Span::raw("└ ")],
            &[Span::raw("  ")],
        );
        assert_eq!(texts(&lines), ["└ one", "  two"]);
    }

    #[test]
    fn truncation_counts_display_width() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("你好世界", 5), "你好…");
        assert_eq!(truncate("abc", 4), "abc");
        let line = truncate_line(Line::from(vec![Span::raw("ab"), Span::raw("cdef")]), 4);
        assert_eq!(plain(&line), "abc…");
    }
}
