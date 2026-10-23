//! 跨会话的界面文字（docs/specs/rust-cli/cross-session.md §2.6）：中文与英文。英文照批准框与
//! 回执警告行的原文，产品名换成 ZCode。
use rust_cli_kernel_api::peers::HoldCause;

use super::i18n::Lang;

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 批准框的标题。
pub fn title(lang: Lang) -> &'static str {
    pick(
        lang,
        "Held message from another session",
        "来自其他会话的消息已扣住",
    )
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
        HoldCause::ModeMismatch => pick(
            lang,
            "The sending session's permission mode class doesn't match this session's, so it wasn't delivered automatically.",
            "发送方会话的权限模式类别与本会话不同，所以没有自动投递。",
        ),
        HoldCause::NoModeAsserted => pick(
            lang,
            "The sender did not attest its permission mode, and this session bypasses permission prompts.",
            "发送方没有声明它的权限模式，而本会话不询问权限。",
        ),
    }
}

/// 正文预览的标题。
pub fn body(lang: Lang) -> &'static str {
    pick(
        lang,
        "Message body (this is what will be delivered):",
        "消息正文（批准后投递的就是这些）：",
    )
}

/// 两个选项：拒绝（缺省焦点）与投递。
pub fn options(lang: Lang) -> [&'static str; 2] {
    [
        pick(
            lang,
            "Deny — drop it and tell the sender it was declined",
            "拒绝——丢弃，并告诉发送方被拒绝",
        ),
        pick(
            lang,
            "Deliver this message to ZCode",
            "把这条消息交给 ZCode",
        ),
    ]
}

/// 还有几条扣着（多于一条时显示）。
pub fn more(lang: Lang, held: usize) -> String {
    match lang {
        Lang::En => format!("{held} held"),
        Lang::Zh => format!("共扣住 {held} 条"),
    }
}

/// 批准框下面的按键提示。
pub fn hint(lang: Lang) -> &'static str {
    pick(
        lang,
        "{↑↓} to choose · {enter} to confirm · {esc} to deny",
        "{↑↓} 选择 · {enter} 确认 · {esc} 拒绝",
    )
}

/// 发送方收到回执后的一行（回执说明的第一句）。
pub fn receipt(lang: Lang, status: &str) -> &'static str {
    match status {
        "held" => pick(
            lang,
            "Cross-session message held by the receiving session. ZCode in that session hasn't seen it.",
            "跨会话消息被对方会话扣住，对方的 ZCode 还没有看到。",
        ),
        "denied" => pick(
            lang,
            "Cross-session message denied. The recipient's user declined it; it was not delivered.",
            "跨会话消息被拒绝：对方用户没有同意，消息没有送达。",
        ),
        "expired" => pick(
            lang,
            "Cross-session message expired without approval. It was never approved, so it wasn't delivered and nothing is waiting in that session.",
            "跨会话消息过期未批准：没有送达，对方会话里也没有留下。",
        ),
        "delivered" => pick(
            lang,
            "Cross-session message released after approval. It was approved and released to that session.",
            "跨会话消息已批准，放行到对方会话。",
        ),
        _ => pick(
            lang,
            "Cross-session message refused. That session is not accepting cross-session messages; it was not delivered.",
            "跨会话消息被拒收：对方会话不接收跨会话消息，消息没有送达。",
        ),
    }
}
