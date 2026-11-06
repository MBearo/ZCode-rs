// 改写自 apps/zcode-host/crates/tui/src/ui/page.rs（feat/rs 79b6691c55）：去掉 `/diff` 的查看器与 `/context`。
//! 独立页面（`/status`、`/help`、`/context`、← 会话面板）：占满整个屏幕，上面是标题，下面是内容，最后一行是提示。
//!
//! 第二级的样子（tui-kit.md §4.3「`/status`、`/help` 独立页面」，自拟，工作线 TK 的 S3）：框架照 Puffin 的设置页
//! （`shell/settings/view.rs` 的 `body`）——第 0 行空、第 1 行从第 3 列起是粗体正文墨的标题、再空一行，正文（产出方已
//! 放在第 3 列起，docs/puffin-tui.md §4.2「网格」），藏在上面或下面的行由 fg3 的 `↑ n more` / `↓ n more` 代替（同设置页的
//! `windowed`）；最后一行是按键提示，从第 3 列起（Puffin 状态栏的 `LEAD`），键 fg2、说明 fg3（按键提示组件）。
//!
//! 一屏的行（docs/specs/rust-cli/tui-kit.md §3.2，工作线 TK 的 C2）：每个全屏视图出**正好 `height` 行** kit 的
//! [`StyledLine`]，交给 `draw.rs` 组装成一帧。[`fit`] 把一屏补齐、截断；按列拼框与光标格用 `frame.rs` 的
//! `overlay` / `caret`。
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::ink::Ink;
use super::lines::truncate_line;

/// 页面的网格：标题、正文与提示从第 3 列起（Puffin 设置页与状态栏的三格缩进）。
pub const LEAD: &str = "   ";

/// 页面的头一行（Puffin 设置页 `shell/settings/view.rs` 的 `body` 的头）：第 3 列起粗体正文墨的标题，后面每一级是
/// line 色的 `  ›  ` 加名字，中间的几级 fg2、最后一级正文墨。
pub fn head(ink: &Ink, title: &str, crumbs: &[&str]) -> StyledLine {
    let mut spans = vec![Span::plain(LEAD), Span::new(title.to_owned(), Style::BOLD)];
    let last = crumbs.len().saturating_sub(1);
    for (index, crumb) in crumbs.iter().enumerate() {
        spans.push(Span::new("  ›  ", ink.theme.line));
        let style = if index == last {
            Style::DEFAULT
        } else {
            ink.theme.fg2
        };
        spans.push(Span::new((*crumb).to_owned(), style));
    }
    StyledLine::new(
        spans
            .into_iter()
            .filter(|span| !span.text.is_empty())
            .collect(),
    )
}

/// 补齐、截断成一屏：正好 `height` 行，每行截到 `width` 列（超出时以 `…` 结尾）。
pub fn fit(mut lines: Vec<StyledLine>, width: u16, height: u16) -> Vec<StyledLine> {
    lines.resize(usize::from(height), StyledLine::EMPTY);
    lines
        .into_iter()
        .map(|line| truncate_line(line, usize::from(width)))
        .collect()
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
