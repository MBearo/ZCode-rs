//! 跨会话通信的文字（docs/specs/rust-cli/cross-session.md §1、§2.5）：给模型的包装、回执、ListAgents 与
//! SendMessage 的固定句子。
//!
//! `prompts/peers/` 下的文件手工维护（来源写在 spec §1 各节），文字逐字固定。

use rust_cli_base::include_lf;

use crate::texts::render;
use crate::trim;

macro_rules! peer_texts {
    ($($name:ident => $file:literal,)*) => {
        $(
            #[doc = concat!("`prompts/peers/", $file, ".txt`")]
            pub fn $name() -> &'static str {
                trim(include_lf!(concat!("../../../prompts/peers/", $file, ".txt")))
            }
        )*

        #[cfg(test)]
        pub(crate) const ALL: &[(&str, fn() -> &'static str)] = &[$(($file, $name)),*];
    };
}

peer_texts! {
    wrap_mid_turn => "wrap_mid_turn",
    wrap_idle => "wrap_idle",
    non_user_prefix => "non_user_prefix",
    receipt_held => "receipt_held",
    receipt_denied => "receipt_denied",
    receipt_expired => "receipt_expired",
    receipt_delivered => "receipt_delivered",
    receipt_refused => "receipt_refused",
    list_self => "list_self",
    list_none => "list_none",
    send_ok => "send_ok",
    send_unknown => "send_unknown",
    send_failed => "send_failed",
}

/// 同伴消息给模型的包装：回合在跑时在工具边界并入（`mid_turn`），空闲时开回合。
pub fn wrap(envelope: &str, mid_turn: bool) -> String {
    let template = if mid_turn {
        wrap_mid_turn()
    } else {
        wrap_idle()
    };
    render(template, &[("envelope", envelope)])
}

/// 没有来源的 meta 输入的前缀：回执并入时加在前面。
pub fn non_user(text: &str) -> String {
    format!("{}\n{text}", non_user_prefix())
}

/// 回执全文里区分状态的短语（重开会话时判断这条回执是否已并入对话）。
pub fn receipt_phrase(status: &str) -> Option<&'static str> {
    Some(match status {
        "held" => " held by that session",
        "denied" => " denied by the recipient user",
        "expired" => " not approved before expiry",
        "delivered" => " approved and released to that session",
        "refused" => " refused",
        _ => return None,
    })
}

/// 一条回执的全文，不含前缀。`status` 是 `held` / `denied` / `expired` / `delivered` / `refused`；
/// `recipients` 是收件方的地址（去重后最多列 3 个），`msg_ids` 最多列 5 个。
pub fn receipt(status: &str, recipients: &[&str], msg_ids: &[&str]) -> Option<String> {
    let template = match status {
        "held" => receipt_held(),
        "denied" => receipt_denied(),
        "expired" => receipt_expired(),
        "delivered" => receipt_delivered(),
        "refused" => receipt_refused(),
        _ => return None,
    };
    let count = msg_ids.len().max(1);
    let subject = if count == 1 {
        "Your message to another session".to_owned()
    } else {
        format!("{count} of your messages to another session")
    };
    let (was, it) = if count == 1 {
        ("was", "it")
    } else {
        ("were", "them")
    };
    let mut unique: Vec<&str> = Vec::new();
    for recipient in recipients.iter().filter(|value| !value.is_empty()) {
        if !unique.contains(recipient) {
            unique.push(recipient);
        }
    }
    let listed = |items: &[&str], limit: usize| {
        let mut text = items
            .iter()
            .take(limit)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        if items.len() > limit {
            text.push_str(", …");
        }
        text
    };
    let who = if unique.is_empty() {
        String::new()
    } else {
        let noun = if unique.len() == 1 {
            "recipient"
        } else {
            "recipients"
        };
        format!("{noun}: {}; ", listed(&unique, 3))
    };
    let details = format!(" ({who}msg_id: {})", listed(msg_ids, 5));
    Some(render(
        template,
        &[
            ("subject", subject.as_str()),
            ("was", was),
            ("it", it),
            ("details", details.as_str()),
        ],
    ))
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
