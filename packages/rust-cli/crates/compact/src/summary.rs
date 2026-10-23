//! 摘要提示与摘要消息的文本（docs/specs/rust-cli/compact.md §3.2、§3.3）。
//!
//! 提示（arm `control`）、摘要格式化、摘要消息、有保留尾段时的说明。

use rust_cli_prompts::compact as texts;
use rust_cli_prompts::texts::render;

/// 摘要提示（arm `control`）。
pub(crate) fn prompt(instructions: Option<&str>) -> String {
    let shape = texts::reply_shape();
    let mut out = render(texts::no_tools(), &[("reply_shape", shape)]);
    out.push_str(texts::control());
    if let Some(extra) = instructions.filter(|text| !text.trim().is_empty()) {
        out.push_str(&render(texts::additional(), &[("instructions", extra)]));
    }
    out.push_str(&render(
        texts::no_tools_reminder(),
        &[("reply_shape", shape)],
    ));
    out
}

/// 去掉 `<analysis>`，`<summary>X</summary>` 换成 `Summary:\nX`，连续空行压成一个。
pub(crate) fn format(raw: &str) -> String {
    let mut text = raw.to_owned();
    if let Some(start) = text.find("<analysis>")
        && let Some(end) = text[start..].find("</analysis>")
    {
        text.replace_range(start..start + end + "</analysis>".len(), "");
    }
    if let Some(start) = text.find("<summary>")
        && let Some(end) = text[start..].find("</summary>")
    {
        let inner = text[start + "<summary>".len()..start + end]
            .trim()
            .to_owned();
        text.replace_range(
            start..start + end + "</summary>".len(),
            &format!("Summary:\n{inner}"),
        );
    }
    collapse_blank_lines(&text).trim().to_owned()
}

/// `/\n\n+/g` → `\n\n`。
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newlines = 0;
    for character in text.chars() {
        if character == '\n' {
            newlines += 1;
            continue;
        }
        match newlines {
            0 => {}
            1 => out.push('\n'),
            _ => out.push_str("\n\n"),
        }
        newlines = 0;
        out.push(character);
    }
    match newlines {
        0 => {}
        1 => out.push('\n'),
        _ => out.push_str("\n\n"),
    }
    out
}

/// 摘要消息怎样写。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SummaryShape {
    /// 摘要部分的开头被裁掉过。
    pub head_truncated: bool,
    /// 有保留尾段。
    pub kept_tail: bool,
}

/// 摘要消息的文本（不追问的写法，有尾段时再接保留尾段的说明）。
/// transcript 路径不写：本工程的记录在 db.sqlite，模型读不到（spec §8）。
pub(crate) fn message_text(raw: &str, shape: SummaryShape) -> String {
    let mut out = format!("{}\n\n{}", texts::summary_header(), format(raw));
    if shape.head_truncated {
        out.push_str("\n\n");
        out.push_str(texts::summary_head_truncated());
    }
    out.push('\n');
    out.push_str(texts::summary_continue());
    if shape.kept_tail {
        out.push_str("\n\n");
        out.push_str(texts::summary_kept_tail());
    }
    out
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
