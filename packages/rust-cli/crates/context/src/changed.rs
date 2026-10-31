//! `edited_text_file` attachment（docs/specs/rust-cli/tools-files.md §4.1）：读过的文件在磁盘上被改动。改动由
//! readFileState 的 owner 查出（kernel-api `ChangedFiles`），这里只渲染。每个文件一条。

use rust_cli_base::chars;
use rust_cli_kernel_api::tool::ChangedFile;
use rust_cli_prompts::texts;
use serde_json::{Value, json};

/// 来源（attachment type）。
pub(crate) const SOURCE: &str = "edited_text_file";

/// 路径进提示词前的转义：尖括号换成实体，控制字符、U+2028 / U+2029 换成 `&#N;`。
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // 控制字符的范围只定义在 base 的 `chars`。
            _ if chars::is_entity_escaped(character) => {
                out.push_str(&format!("&#{};", u32::from(character)));
            }
            _ => out.push(character),
        }
    }
    out
}

/// 一个改动的文件：有片段时带上片段，没有时只说改了。
pub(crate) fn render(change: &ChangedFile) -> (String, Value) {
    let path = escape(&change.path);
    let note = texts::render(texts::edited_text_file(), &[("path", &path)]);
    let text = if change.snippet.is_empty() {
        texts::render(texts::edited_text_file_hidden(), &[("note", &note)])
    } else {
        texts::render(
            texts::edited_text_file_changes(),
            &[("note", &note), ("snippet", &change.snippet)],
        )
    };
    (
        text,
        json!({"filename": change.path, "snippet": change.snippet}),
    )
}

#[cfg(test)]
#[path = "changed_tests.rs"]
mod tests;
