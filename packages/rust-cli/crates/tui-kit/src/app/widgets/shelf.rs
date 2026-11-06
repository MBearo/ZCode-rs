//! 命令架（docs/puffin-tui.md §4.3「命令」；docs/specs/rust-cli/tui-kit.md §4.4）：草稿是 `/词` 时输入框上方的命令，每个一行，
//! 名字一列然后说明；选中的是 brand，现在不能运行的是灰色并说原因；至多 16 行，藏起来的用 `↑ n more` / `↓ n more` 代替。
//! 纯的；入参是中性的 [`ShelfRow`]。匹配（名字或别名的开头）也在这里。
//! 移植自 Puffin 的 shell/composer.rs（`shelf_rows`）、shell/shelf.rs（`shelf_matches` 的过滤）与
//! shell/consts.rs（`SHELF_ROWS`、`SHELF_NAME_MAX`、`shelf_word`）。
use crate::app::element::{Element, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::{clip, measured, pad};
use super::lists::{chosen_style, window};

/// 命令架最多显示的行数，含 more 行。
pub const SHELF_ROWS: usize = 16;
/// 更长的命令名（带 `/`）用 `…` 截断。
pub const SHELF_NAME_MAX: usize = 24;

/// 草稿只有 `/` 与一个词时的那个词（命令架按它过滤）。
pub fn word(text: &str) -> Option<&str> {
    let word = text.strip_prefix('/')?;
    (!word.chars().any(char::is_whitespace)).then_some(word)
}

/// 命令 `name` 在打了（小写的）`typed` 时上架：名字以它开头，或它是指向这个命令的别名的开头。
pub fn matches(name: &str, aliases: &[(&str, &str)], typed: &str) -> bool {
    name.starts_with(typed)
        || aliases
            .iter()
            .any(|(alias, to)| *to == name && alias.starts_with(typed))
}

/// 架上的一个命令：名字（不带 `/`）、说明，与现在不能运行的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShelfRow<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub unavailable: Option<&'a str>,
}

/// 命令架的行：`rows` 是匹配的命令，`choice` 是选中的那个。没有匹配时一行说 ⏎ 把它当消息发出。
pub fn rows(theme: &Theme, rows: &[ShelfRow<'_>], choice: usize) -> Vec<Element> {
    let indent = Span::new(INDENT, Style::DEFAULT);
    if rows.is_empty() {
        return vec![single_line(
            vec![
                indent,
                Span::new("No command matches. ", theme.fg3),
                Span::new("⏎", theme.fg2),
                Span::new(" sends it as a message.", theme.fg3),
            ],
            false,
        )];
    }
    let name_width = rows
        .iter()
        .map(|c| string_width(c.name) + 1)
        .max()
        .unwrap_or(0)
        .min(SHELF_NAME_MAX)
        + 3;
    // 最多 SHELF_ROWS 行：窗口跟着选中的行，被藏起来的用「↑ n more」/「↓ n more」代替最上或最下的一行。
    let (from, until) = window(rows.len(), choice.min(rows.len() - 1), SHELF_ROWS);
    let more = |text: String| {
        measured(single_line(
            vec![indent.clone(), Span::new(text, theme.fg3)],
            false,
        ))
    };
    let mut out = Vec::new();
    if from > 0 {
        out.push(more(format!("↑ {from} more")));
    }
    for (i, c) in rows.iter().enumerate().take(until).skip(from) {
        let name = pad(&clip(&format!("/{}", c.name), SHELF_NAME_MAX), name_width);
        let description = c
            .description
            .strip_suffix('.')
            .unwrap_or(c.description)
            .to_owned();
        let spans = match c.unavailable {
            Some(why) => vec![
                Span::new(name, theme.fg3),
                Span::new(why.to_owned(), theme.fg3.with_attr(attr::ITALIC)),
            ],
            None if i == choice => vec![
                Span::new(name, chosen_style(theme)),
                Span::new(description, theme.fg2),
            ],
            None => vec![
                Span::new(name, theme.fg2),
                Span::new(description, theme.fg3),
            ],
        };
        let mut row = vec![indent.clone()];
        row.extend(spans);
        out.push(measured(single_line(row, false)));
    }
    if until < rows.len() {
        out.push(more(format!("↓ {} more", rows.len() - until)));
    }
    out
}
