//! 栈（docs/puffin-tui.md §4.2「发出的消息」的「栈」；docs/specs/rust-cli/tui-kit.md §4.4）：⇥ 推上去、下个回合才发的消息，
//! 在输入框里草稿的上面，最早的在上。`›` fg3、字 fg2（说了，没发），每条一行，用 `…` 截断；最上面一行在槽的右沿说
//! `next turn`。至多 [`STACK_ROWS`] 行：再多时 `N earlier` 领着最新的三条。纯的；入参是每条消息打出来的字。
//! 移植自 Puffin 的 shell/composer.rs（`stack_rows`）与 shell/consts.rs（`STACK_ROWS`、`STACK_LABEL`、
//! `COMPOSER_INDENT`）。
use crate::app::element::{Element, single_line};
use crate::app::frame::PROMPT_ARROW;
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fit::clip;
use super::key_hints::{edge, on_row};

/// 栈最多的行数。
pub const STACK_ROWS: usize = 4;
/// 栈最上面一行右沿的说明。
pub const STACK_LABEL: &str = "next turn";
/// 输入框里的字从第 3 列起。
pub const COMPOSER_INDENT: &str = "   ";

/// 栈的行：`texts` 是推上去的消息打出来的样子（提及已写回打出的字），最早的在前；宽 `width`。
pub fn rows(theme: &Theme, texts: &[&str], width: usize) -> Vec<Element> {
    if texts.is_empty() {
        return Vec::new();
    }
    let shown = if texts.len() > STACK_ROWS {
        &texts[texts.len() - (STACK_ROWS - 1)..]
    } else {
        texts
    };
    let earlier = texts.len() - shown.len();
    let label = vec![Span::new(STACK_LABEL, theme.fg3)];
    // 最上面的一行留出说明与它前面的两格（on_row）；其余到最后一列之前。
    let cols = |top: bool| {
        let room = if top {
            edge(width).saturating_sub(2 + string_width(STACK_LABEL))
        } else {
            width.saturating_sub(1)
        };
        room.saturating_sub(COMPOSER_INDENT.len()).max(1)
    };
    let row = |spans: Vec<Span>, top: bool| {
        if top {
            on_row(single_line(spans, false), label.clone())
        } else {
            single_line(spans, false)
        }
    };
    let mut out = Vec::new();
    if earlier > 0 {
        out.push(row(
            vec![
                Span::new(COMPOSER_INDENT, Style::DEFAULT),
                Span::new(format!("{earlier} earlier"), theme.fg3),
            ],
            true,
        ));
    }
    for (i, text) in shown.iter().enumerate() {
        let top = earlier == 0 && i == 0;
        // 换行画成空格（§4.2「发出的消息」的「栈」）。
        let words = clip(&text.replace('\n', " "), cols(top));
        out.push(row(
            vec![
                Span::new(" ", Style::DEFAULT),
                Span::new(PROMPT_ARROW, theme.fg3),
                Span::new(" ", Style::DEFAULT),
                Span::new(words, theme.fg2),
            ],
            top,
        ));
    }
    out
}
