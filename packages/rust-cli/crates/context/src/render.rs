//! 渲染用的小工具（列表与转义）。

use rust_cli_base::chars;
use rust_cli_kernel_api::message::{AttachmentMessage, Message, SystemMessage};

/// DEL（U+007F）。
const DEL: char = '\u{7f}';

/// 一行文本或一组子项（字符串是 ` - x`，数组的每项是 `  - x`）。
pub(crate) enum Bullet {
    Line(String),
    Nested(Vec<String>),
}

pub(crate) fn bullets(items: Vec<Bullet>) -> String {
    let mut lines = Vec::new();
    for item in items {
        match item {
            Bullet::Line(line) => lines.push(format!(" - {line}")),
            Bullet::Nested(children) => {
                lines.extend(children.into_iter().map(|child| format!("  - {child}")));
            }
        }
    }
    lines.join("\n")
}

/// 路径等外部值进提示词前的转义：尖括号换成实体，
/// C0 控制字符与 DEL 换成 `&#N;`。
pub(crate) fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // C0 的范围只定义在 base 的 `chars`；这里另加 DEL，不含 C1。
            c if chars::is_c0(c) || c == DEL => {
                out.push_str(&format!("&#{};", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// 最近一次压缩边界之后的历史。
pub(crate) fn since_boundary(history: &[Message]) -> &[Message] {
    let start = history
        .iter()
        .rposition(|message| matches!(message, Message::System(SystemMessage::CompactBoundary(_))))
        .map_or(0, |index| index + 1);
    &history[start..]
}

/// 历史里某个来源的 attachment，按出现顺序。
pub(crate) fn attachments<'a>(
    history: &'a [Message],
    source: &'a str,
) -> impl DoubleEndedIterator<Item = &'a AttachmentMessage> {
    history.iter().filter_map(move |message| match message {
        Message::Attachment(attachment) if attachment.source == source => Some(attachment),
        _ => None,
    })
}
