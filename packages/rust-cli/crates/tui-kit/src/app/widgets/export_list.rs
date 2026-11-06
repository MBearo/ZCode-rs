//! `/export` 的列表（docs/specs/rust-cli/tui-manage.md §3；docs/specs/rust-cli/tui-kit.md §4.4）：与 `/mode` 同一层的
//! 两行，复制到剪贴板与存成文件；没有头。纯的。
//! 移植自 Puffin 的 shell/export.rs（`EXPORT_ROWS`、`export_rows`）。
use crate::app::element::Element;
use crate::render::Span;
use crate::theme::theme::Theme;

use super::fit::pad;
use super::lists::{chosen_style, row};

/// `/export` 列表的两行：名字与说明。
pub const EXPORT_ROWS: [(&str, &str); 2] = [
    (
        "Copy to clipboard",
        "Copy the conversation to your clipboard",
    ),
    (
        "Save to file",
        "Save the conversation to a file in this folder",
    ),
];

/// `/export` 列表的行。
pub fn rows(theme: &Theme, choice: usize) -> Vec<Element> {
    EXPORT_ROWS
        .iter()
        .enumerate()
        .map(|(i, (label, desc))| {
            let chosen = i == choice;
            row(vec![
                Span::new(
                    pad(label, 21),
                    if chosen {
                        chosen_style(theme)
                    } else {
                        theme.fg2
                    },
                ),
                Span::new(*desc, if chosen { theme.fg2 } else { theme.fg3 }),
            ])
        })
        .collect()
}
