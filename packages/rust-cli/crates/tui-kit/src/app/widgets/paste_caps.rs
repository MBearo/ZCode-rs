//! 大段粘贴的帽与预览（docs/puffin-tui.md §4.2「粘贴」；docs/specs/rust-cli/tui-kit.md §4.4）：大小的说法（`214 lines`、
//! `41.2k chars`、`41.2 KB`）、帽（`Pasted · <大小>`，彩色在线的墨色上，plain 是方括号；进草稿时从板上淡入，光标下反显）、
//! 文字的预览（`│` 轨上的前几行与还有多少）与板里发出的文件的行。纯的；入参是大小的字与原文，壳持有粘贴与草稿。
//! 移植自 Puffin 的 shell/pastes.rs（Pastes.scala 第 1–319 行）的帽与预览。
use crate::app::element::{Element, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::{display_width, string_width};
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{MEASURE_MARGIN, MEASURE_MAX, clip, fixed, measured, tokens};
use super::skill_panel::faded;

/// 预览至多显示的行。
pub const PEEK_LINES: usize = 5;
/// 预览的键，槽这样显示。
pub const KEYS: [(&str, &str); 1] = [("^D", "remove")];

/// `lines` 行、`chars` 个字的粘贴：`214 lines`、`41.2k chars`。
pub fn size(lines: usize, chars: usize) -> String {
    if lines >= 2 {
        format!("{} lines", commas(lines))
    } else {
        format!("{} chars", tokens(chars as f64))
    }
}

/// 不知道行数时文件的大小：`41.2 KB`。
pub fn bytes(n: f64) -> String {
    if n < 1024.0 {
        return format!("{} B", n as i64);
    }
    let (v, unit) = if n < 1024.0 * 1024.0 {
        (n / 1024.0, "KB")
    } else {
        (n / 1024.0 / 1024.0, "MB")
    };
    let s = fixed(v, 1);
    format!("{} {unit}", s.strip_suffix(".0").unwrap_or(&s))
}

/// 千位用逗号分开：`12,345`。
pub fn commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 文字的粘贴的标签：`Pasted · <大小>`。
pub fn label(size: &str) -> String {
    format!("Pasted · {size}")
}

/// 帽在宽 `width` 的行里占的格：标签与两边各一格，标签比一行（W − 1 列）宽时只有大小。
pub fn cap_width_of(size: &str, width: usize) -> usize {
    string_width(&size_cap(size, width)) + 2
}

/// 大小为 `size` 的帽的字：`Pasted · <size>`，比宽 `width` 的一行宽时只有大小。
pub fn size_cap(size: &str, width: usize) -> String {
    let full = label(size);
    if string_width(&full) + 2 < width {
        full
    } else {
        size.to_owned()
    }
}

/// 字是 `text` 的帽，在宽的行里（§4.2「粘贴」）：底色与标签作为一整块从板上淡入了 `age` 个 tick。`under`：在光标下，
/// 整个帽反显，记作光标。`reading`：图片还在读，标签是 fg3。
pub fn cap(theme: &Theme, text: &str, under: bool, reading: bool, age: i64) -> Vec<Span> {
    if under {
        let shown = if theme.rich {
            format!(" {text} ")
        } else {
            format!("[{text}]")
        };
        return vec![Span::new(shown, Style::CARET)];
    }
    let rest = if reading { theme.fg3 } else { theme.fg2 };
    fade::fade_in(
        theme,
        spans(parts(theme, text, rest)),
        age,
        fade::TICKS,
        theme.panel,
    )
}

/// 大小为 `size` 的落定的帽（发出的文件的行）。
pub fn cap_spans(theme: &Theme, size: &str, width: usize) -> Vec<Span> {
    spans(parts(theme, &size_cap(size, width), Style::DEFAULT))
}

/// 字是 `text` 的落定的帽（发出的板上图片的帽）。
pub fn label_spans(theme: &Theme, text: &str) -> Vec<Span> {
    spans(parts(theme, text, Style::DEFAULT))
}

fn spans(parts: Vec<(String, Style)>) -> Vec<Span> {
    parts.into_iter().map(|(t, st)| Span::new(t, st)).collect()
}

/// 彩色：标签在线的墨色上，`Pasted` 与大小 fg2（大小用 `rest`），点 fg3。plain：没有底色，暗的方括号。
fn parts(theme: &Theme, text: &str, rest: Style) -> Vec<(String, Style)> {
    if !theme.rich {
        return vec![
            ("[".to_owned(), theme.fg3),
            (text.to_owned(), Style::DEFAULT),
            ("]".to_owned(), theme.fg3),
        ];
    }
    let ground = theme.line.fg_color();
    match text.find(" · ") {
        None => vec![(format!(" {text} "), theme.fg2.with_bg(ground))],
        Some(k) => {
            let tail = if rest == Style::DEFAULT {
                theme.fg2
            } else {
                rest
            };
            vec![
                (format!(" {}", &text[..k]), theme.fg2.with_bg(ground)),
                (" · ".to_owned(), theme.fg3.with_bg(ground)),
                (
                    format!("{} ", &text[k + " · ".len()..]),
                    tail.with_bg(ground),
                ),
            ]
        }
    }
}

// ---- 预览 ----

/// 终端宽 `width` 下文字的粘贴（原文是 `body`，不带末尾换行）的预览，打开之后 `age` 个 tick：`│` 轨上 fg2 的前几行，用 `…`
/// 截断，再 fg3 说还有多少；一行长的切成几行（§4.2「粘贴」）。行一起淡入，同 `$` 面板。
pub fn text_peek(theme: &Theme, body: &str, width: usize, age: i64) -> Vec<Element> {
    // 与其他面板的行一样按 measure 的宽度（两列边距）切，`   │ ` 占 5 列。
    let room = width
        .saturating_sub(MEASURE_MARGIN)
        .min(MEASURE_MAX)
        .saturating_sub(5)
        .max(1);
    let shown: Vec<String> = if body.contains('\n') {
        let ls: Vec<&str> = body.split('\n').collect();
        let mut first: Vec<String> = ls
            .iter()
            .take(PEEK_LINES)
            .map(|l| clip(&l.replace('\t', "  "), room))
            .collect();
        first.push(if ls.len() > PEEK_LINES {
            format!("… {} more lines", commas(ls.len() - PEEK_LINES))
        } else {
            format!("{} lines", commas(ls.len()))
        });
        first
    } else {
        let line = body.replace('\t', "  ");
        let mut rows = Vec::new();
        let mut at = 0;
        while rows.len() < PEEK_LINES - 1 && at < line.len() {
            let (piece, next) = take(&line, at, room);
            rows.push(piece.to_owned());
            at = next;
        }
        let left = line[at..].chars().count();
        rows.push(if left > 0 {
            format!("… {} more chars", tokens(left as f64))
        } else {
            format!("{} chars", tokens(line.chars().count() as f64))
        });
        rows
    };
    let last = shown.len() - 1;
    shown
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            let row = vec![
                Span::new(INDENT, Style::DEFAULT),
                Span::new("│", theme.line),
                Span::new(" ", Style::DEFAULT),
                Span::new(t, if i == last { theme.fg3 } else { theme.fg2 }),
            ];
            measured(single_line(faded(theme, row, age), false))
        })
        .collect()
}

/// `s` 从字节 `at` 起放得进 `room` 格的部分，与其余的从哪里开始。
fn take(s: &str, at: usize, room: usize) -> (&str, usize) {
    let mut i = at;
    let mut c = 0;
    for ch in s[at..].chars() {
        let w = display_width(ch);
        if c + w > room {
            break;
        }
        c += w;
        i += ch.len_utf8();
    }
    (&s[at..i], i)
}

/// 板里一个文件的行：帽，两格，fg3 的 `attached as` 与它的名字。
/// `file_name` 是它发出的名字，`size` 是它的大小的字。
pub fn attached_row(
    theme: &Theme,
    file_name: &str,
    size: &str,
    lead: Vec<Span>,
    width: usize,
) -> Element {
    let mut row = lead;
    row.extend(cap_spans(theme, size, width));
    row.push(Span::new("  ", Style::DEFAULT));
    row.push(Span::new(format!("attached as {file_name}"), theme.fg3));
    single_line(row, false)
}
