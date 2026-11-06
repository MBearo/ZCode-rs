//! 把文字切成样式段，解释其中嵌的 SGR 序列。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Layout.scala 的 `tokenize` / `applySgr`（feat/tui a2bf878b34）。
//!
//! 与 Scala 的差别只在畸形的序列上：参数在第一个字母处结束，字母用 `char::is_alphabetic`（Scala 是 UTF-16 单元上的
//! `Character.isLetter`）；颜色参数按 `u8` 截断（Scala 把超过 255 的值打包进 24 位）。我们自己的 `render` 不会产生它们。
use std::borrow::Cow;

use crate::render::{Color, Span, Style, attr};

const ESC: char = '\u{1b}';

/// 快路径（没有转义）是一段，文字原样（借用的不复制）。
pub(super) fn tokenize(text: Cow<'static, str>, base: Style) -> Vec<Span> {
    if !text.contains(ESC) {
        return vec![Span::new(text, base)];
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut style = base;
    let mut i = 0usize;
    while i < n {
        let ch = chars[i];
        if ch == ESC && i + 1 < n && chars[i + 1] == '[' {
            if !run.is_empty() {
                spans.push(Span::new(std::mem::take(&mut run), style));
            }
            let mut j = i + 2;
            let mut params = String::new();
            while j < n && !chars[j].is_alphabetic() {
                params.push(chars[j]);
                j += 1;
            }
            let final_byte = if j < n { chars[j] } else { 'm' };
            j += 1;
            if final_byte == 'm' {
                style = apply_sgr(&params, base, style);
            }
            i = j;
        } else {
            run.push(ch);
            i += 1;
        }
    }
    if !run.is_empty() {
        spans.push(Span::new(run, style));
    }
    spans
}

/// 把一串 SGR 参数（`"0;1;36"`）套到样式上。`0` 重置到 `base`：元素自己的样式保持是底。
fn apply_sgr(params: &str, base: Style, current: Style) -> Style {
    if params.is_empty() || params == "0" {
        return base;
    }
    // Java 的 `split(";")`：去掉末尾的空段。解析不了的参数算 0（Scala 的 `toIntOption.getOrElse(0)`）。
    let mut parts: Vec<&str> = params.split(';').collect();
    while parts.last() == Some(&"") {
        parts.pop();
    }
    let codes: Vec<i32> = parts
        .iter()
        .map(|s| {
            if s.is_empty() {
                0
            } else {
                s.parse::<i32>().unwrap_or(0)
            }
        })
        .collect();
    let byte = |v: i32| v as u8;
    let mut st = current;
    let mut k = 0usize;
    while k < codes.len() {
        match codes[k] {
            0 => st = base,
            1 => st = st.with_attr(attr::BOLD),
            2 => st = st.with_attr(attr::DIM),
            3 => st = st.with_attr(attr::ITALIC),
            4 => st = st.with_attr(attr::UNDERLINE),
            7 => st = st.with_attr(attr::REVERSE),
            39 => st = st.with_fg(Color::Default),
            49 => st = st.with_bg(Color::Default),
            x @ 30..=37 => st = st.with_fg(Color::Named(byte(x - 30))),
            x @ 90..=97 => st = st.with_fg(Color::Named(byte(8 + x - 90))),
            x @ 40..=47 => st = st.with_bg(Color::Named(byte(x - 40))),
            x @ 100..=107 => st = st.with_bg(Color::Named(byte(8 + x - 100))),
            c @ (38 | 48) => {
                let fg = c == 38;
                let color = if k + 2 < codes.len() && codes[k + 1] == 5 {
                    let c = Color::Indexed(byte(codes[k + 2]));
                    k += 2;
                    Some(c)
                } else if k + 4 < codes.len() && codes[k + 1] == 2 {
                    let c = Color::TrueColor(
                        byte(codes[k + 2]),
                        byte(codes[k + 3]),
                        byte(codes[k + 4]),
                    );
                    k += 4;
                    Some(c)
                } else {
                    None
                };
                if let Some(c) = color {
                    st = if fg { st.with_fg(c) } else { st.with_bg(c) };
                }
            }
            _ => {}
        }
        k += 1;
    }
    st
}
