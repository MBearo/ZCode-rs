//! 按键提示组件（docs/specs/rust-cli/tui.md §7.4，修复线 F26，用户决定；规则参照 Codex `key_hint.rs` 与
//! `footer_hint.rs`，c3d3b142d）：提示写成模板，`{键}` 标出按键（中英文的词序都能放）；按键用 `key` 样式，
//! 其余文字（说明、` · ` 分隔）用 `secondary` 样式。所有按键引导（footer、列表与弹窗底部、页面与面板、对话框、
//! 快捷键面板）都经这里画，样式只在 `Palette` 的记号里。
use ratatui::text::{Line, Span};

use super::palette::Palette;

/// 模板的片段：（文字，是不是按键）。没有配对的 `{` 按普通文字处理。
fn parts(template: &str) -> Vec<(String, bool)> {
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|i| open + i) else {
            break;
        };
        if open > 0 {
            parts.push((rest[..open].to_owned(), false));
        }
        parts.push((rest[open + 1..close].to_owned(), true));
        rest = &rest[close + 1..];
    }
    if !rest.is_empty() {
        parts.push((rest.to_owned(), false));
    }
    parts
}

/// 模板去掉标记后的文字（量宽度、找点击位置用）。
pub fn plain(template: &str) -> String {
    parts(template).into_iter().map(|(text, _)| text).collect()
}

/// 模板的片段：按键 `key`，其余 `secondary`。
pub fn spans(template: &str, palette: &Palette) -> Vec<Span<'static>> {
    parts(template)
        .into_iter()
        .map(|(text, key)| {
            let style = if key {
                palette.key()
            } else {
                palette.secondary()
            };
            Span::styled(text, style)
        })
        .collect()
}

/// 一行提示：前面缩进 `indent` 个空格。
pub fn line(indent: usize, template: &str, palette: &Palette) -> Line<'static> {
    let mut all = vec![Span::raw(" ".repeat(indent))];
    all.extend(spans(template, palette));
    Line::from(all)
}

/// 几条提示之间的分隔（` · `，`secondary`）。
pub fn separator(palette: &Palette) -> Span<'static> {
    Span::styled(" · ", palette.secondary())
}

#[cfg(test)]
#[path = "key_hint_tests.rs"]
mod tests;
