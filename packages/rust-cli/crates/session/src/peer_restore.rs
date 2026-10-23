//! 重开会话时恢复跨会话的状态（docs/specs/rust-cli/cross-session.md §2.2、§3.3）：扣住的消息、接收了但还没并入
//! 对话的消息与回执。是否已并入以对话里的文字为准（与后台通知的去重相同，lane.rs）。

use std::collections::HashMap;

use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::peers::{
    HeldPeerMessage, HoldCause, PeerReceipt, ReceiptStatus, parse_envelope,
};

use crate::actor::Actor;
use crate::lane::{Lane, Waiting};
use crate::peers::{
    Held, MESSAGE_ENTRY, PeerItem, RECEIPT_ENTRY, RESOLVED_ENTRY, address, outcome_of, text_field,
};

/// 对话里每条消息的文字（attachment 的 text、user 消息的文本块）。
fn conversation(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Attachment(attachment) => Some(attachment.text.clone()),
            Message::User(user) => Some(
                user.content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

fn cause(text: &str) -> Option<HoldCause> {
    match text {
        "mode-mismatch" => Some(HoldCause::ModeMismatch),
        "no-mode-asserted" => Some(HoldCause::NoModeAsserted),
        _ => None,
    }
}

pub(crate) async fn restore(actor: &mut Actor) {
    let store = actor.deps.store.clone();
    let id = actor.meta.id.clone();
    let read = |kind: &'static str| {
        let (store, id) = (store.clone(), id.clone());
        async move {
            store.entries(&id, kind).await.unwrap_or_else(|error| {
                tracing::warn!(%error, kind, "cross-session entries could not be read");
                Vec::new()
            })
        }
    };
    let messages = read(MESSAGE_ENTRY).await;
    let receipts = read(RECEIPT_ENTRY).await;
    if messages.is_empty() && receipts.is_empty() {
        return;
    }
    let resolved: HashMap<String, Option<ReceiptStatus>> = read(RESOLVED_ENTRY)
        .await
        .into_iter()
        .map(|(msg_id, data)| (msg_id, outcome_of(&data)))
        .collect();
    let seen = conversation(&actor.messages);
    let merged = |item: &PeerItem| {
        let (text, turn) = (item.text(), item.turn_text());
        seen.iter().any(|known| *known == text || *known == turn)
    };
    for (msg_id, data) in messages {
        actor.peers.seen.insert(msg_id.clone());
        let Some(envelope) = text_field(&data, "envelope") else {
            continue;
        };
        let hold = data.get("held").filter(|value| !value.is_null());
        let still_held = hold.is_some() && !resolved.contains_key(&msg_id);
        if still_held {
            let hold = hold.cloned().unwrap_or_default();
            let parsed = parse_envelope(&envelope);
            let Some(from) = address(&data, "from") else {
                continue;
            };
            actor.peers.held.push(Held {
                view: HeldPeerMessage {
                    msg_id: msg_id.clone(),
                    from,
                    from_name: text_field(&data, "fromName"),
                    body: parsed.map(|envelope| envelope.body).unwrap_or_default(),
                    cause: text_field(&hold, "cause")
                        .and_then(|text| cause(&text))
                        .unwrap_or(HoldCause::ModeMismatch),
                    received_at_ms: data["receivedAt"].as_u64().unwrap_or_default(),
                    expires_at_ms: hold["expiresAt"].as_u64().unwrap_or_default(),
                },
                envelope,
            });
            continue;
        }
        let delivered =
            hold.is_none() || resolved.get(&msg_id) == Some(&Some(ReceiptStatus::Delivered));
        let item = PeerItem::Message { msg_id, envelope };
        if delivered && !merged(&item) {
            actor.lane.push_back(Lane::Notice(Waiting::Peer(item)));
        }
    }
    // 没并入对话的回执：放回合并表，下一次醒来（立即到期）时交出。对话里有同一 msg_id 的这种回执就算已并入。
    for (key, data) in receipts {
        actor.peers.seen.insert(key);
        let (Some(msg_id), Some(status), Some(recipient)) = (
            text_field(&data, "msgId"),
            text_field(&data, "status").and_then(|text| ReceiptStatus::parse(&text)),
            address(&data, "recipient"),
        ) else {
            continue;
        };
        let receipt = PeerReceipt {
            seq: 0,
            msg_id,
            status,
            recipient,
        };
        let phrase = rust_cli_prompts::peers::receipt_phrase(status.as_str()).unwrap_or_default();
        let shown = seen
            .iter()
            .any(|known| known.contains(&receipt.msg_id) && known.contains(phrase));
        if !shown {
            actor.peers.receipts.push(receipt);
            actor.peers.receipts_due = Some(actor.deps.clock.now_ms());
        }
    }
    tracing::info!(
        event = "peers.restored",
        held = actor.peers.held.len(),
        "cross-session state restored"
    );
    // 打开时的模式可能已经不同；已过期的立即按过期结算（回执发给发送方的地址）。
    actor.peers_mode_changed(false).await;
    actor.peers_expire(false).await;
    if !actor.peers.held.is_empty() {
        actor.emit_held();
    }
}
