//! 跨会话的界面文字（docs/specs/rust-cli/cross-session.md §2.6）：中文与英文。英文照批准框与
//! 回执警告行的原文，产品名换成 ZCode。
use rust_cli_kernel_api::peers::HoldCause;

use super::i18n::Lang;

/// 批准的板头（照 Puffin 请求的问句，`Run this command?`）。以前是 `Held message from another session`。
pub fn title(lang: Lang) -> &'static str {
    lang.pick("Deliver this message?", "投递这条消息？")
}

/// 第一行：发送方的地址与它自称的名字。
pub fn from(lang: Lang, address: &str, name: Option<&str>) -> String {
    match (lang, name) {
        (Lang::En, Some(name)) => {
            format!(
                "Another ZCode session sent a message: from {address} (peer claims name: {name})"
            )
        }
        (Lang::En, None) => format!("Another ZCode session sent a message: from {address}"),
        (Lang::Zh, Some(name)) => {
            format!("另一个 ZCode 会话发来消息：来自 {address}（对方自称 {name}）")
        }
        (Lang::Zh, None) => format!("另一个 ZCode 会话发来消息：来自 {address}"),
    }
}

/// 扣住的原因。
pub fn cause(lang: Lang, cause: HoldCause) -> &'static str {
    match cause {
        HoldCause::ModeMismatch => lang.pick("The sending session's permission mode class doesn't match this session's, so it wasn't delivered automatically.",
            "发送方会话的权限模式类别与本会话不同，所以没有自动投递。",
        ),
        HoldCause::NoModeAsserted => lang.pick("The sender did not attest its permission mode, and this session bypasses permission prompts.",
            "发送方没有声明它的权限模式，而本会话不询问权限。",
        ),
    }
}

/// 两个回答（照 Puffin 回答的名字与说明两列，肯定的在前）：投递与拒绝，各是（名字，说明）。以前是一句话的选项，拒绝在前。
pub fn options(lang: Lang) -> [(&'static str, &'static str); 2] {
    [
        (
            lang.pick("Deliver", "投递"),
            lang.pick(
                "the agent reads it as a message",
                "agent 把它当作一条消息读",
            ),
        ),
        (
            lang.pick("Deny", "拒绝"),
            lang.pick("drop it and tell the sender", "丢弃，并告诉发送方"),
        ),
    ]
}
