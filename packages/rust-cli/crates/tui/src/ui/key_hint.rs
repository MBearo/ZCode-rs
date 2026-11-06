//! 按键提示组件（docs/specs/rust-cli/tui.md §7.4，修复线 F26；画法 tui-kit.md §3.8 的转换示例）：提示写成模板，`{键}` 标出
//! 按键（中英文的词序都能放）；按键用 `key` 墨色，其余文字（说明、` · ` 分隔）用 `secondary`。所有按键引导（footer、列表
//! 与弹窗底部、页面与面板、对话框、快捷键面板）都经这里画。
//!
//! 墨色照 Puffin 的槽（docs/puffin-tui.md §4.2「按键提示（槽）」，`shell/key_hints.rs` 的 `spans`）：键 `fg2`、标签 `fg3`，
//! 经 [`Ink`] 取。模板的语法与 ` · ` 分隔是旧界面的，不变（tui-kit.md §1.2）。
//!
//! 只产出 kit 的 [`Span`] / [`StyledLine`]（tui-kit.md §3.8）。
use rust_cli_tui_kit::render::Span;

use super::ink::Ink;

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
pub fn spans(template: &str, ink: &Ink) -> Vec<Span> {
    parts(template)
        .into_iter()
        .map(|(text, key)| {
            let style = if key { ink.key() } else { ink.secondary() };
            Span::new(text, style)
        })
        .collect()
}

#[cfg(test)]
#[path = "key_hint_tests.rs"]
mod tests;
