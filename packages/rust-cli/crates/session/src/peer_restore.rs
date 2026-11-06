//! 重开会话时恢复跨会话的状态（docs/specs/rust-cli/cross-session.md §2.2、§3.3）：扣住的消息、接收了但还没并入
//! 对话的消息与回执。是否已并入看 `peer_merged`（并入时与那条消息同一批提交，F139）；只有旧会话的 entry（没有
//! `mergeTracked`）才在完整历史里按文字找一次，找到了补记事实。
//!
//! 修 bug（F139）：以前一律按文字找，而且只找模型侧的历史（最后一个压缩点起）；压缩之前并入的消息与回执每次重开都
//! 被当成没并入，再投递一次。

use std::collections::{HashMap, HashSet};

use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::peers::{
    HeldPeerMessage, HoldCause, PeerReceipt, ReceiptStatus, parse_envelope,
};
use rust_cli_kernel_api::store::{Fact, entry_kinds};

use crate::actor::Actor;
use crate::lane::Waiting;
use crate::peers::{
    Held, PeerItem, address, merge_tracked, merged_message_id, merged_receipt, merged_receipt_id,
    outcome_of, text_field,
};

/// 对话里每条消息的文字（attachment 的 text、user 消息的文本块）。
fn conversation<'a>(messages: impl Iterator<Item = &'a Message>) -> Vec<String> {
    messages
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

/// 旧会话的 entry 的回退（F139 之前的规则）：完整历史（压缩点之前只供显示的一段接上模型侧的历史，compact.md §5.1）
/// 的文字只在第一次用到时取一次；找到的记下要补的事实。
struct Legacy<'a> {
    history: [&'a [Message]; 2],
    texts: Option<Vec<String>>,
    backfill: Vec<Fact>,
}

impl<'a> Legacy<'a> {
    fn new(earlier: &'a [Message], messages: &'a [Message]) -> Self {
        Self {
            history: [earlier, messages],
            texts: None,
            backfill: Vec::new(),
        }
    }

    fn texts(&mut self) -> &[String] {
        let history = self.history;
        self.texts
            .get_or_insert_with(|| conversation(history.into_iter().flatten()))
    }

    /// 对话里有这条消息并入的两种写法之一。
    fn message_shown(&mut self, item: &PeerItem) -> bool {
        let (text, turn) = (item.text(), item.turn_text());
        self.texts()
            .iter()
            .any(|known| *known == text || *known == turn)
    }

    /// 对话里有一条文字同时含这条回执的 msg_id 与它的状态短语。
    fn receipt_shown(&mut self, receipt: &PeerReceipt) -> bool {
        let phrase =
            rust_cli_prompts::peers::receipt_phrase(receipt.status.as_str()).unwrap_or_default();
        self.texts()
            .iter()
            .any(|known| known.contains(&receipt.msg_id) && known.contains(phrase))
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
    let messages = read(entry_kinds::PEER_MESSAGE).await;
    let receipts = read(entry_kinds::PEER_RECEIPT).await;
    if messages.is_empty() && receipts.is_empty() {
        return;
    }
    let resolved: HashMap<String, Option<ReceiptStatus>> = read(entry_kinds::PEER_RESOLVED)
        .await
        .into_iter()
        .map(|(msg_id, data)| (msg_id, outcome_of(&data)))
        .collect();
    let merged: HashSet<String> = read(entry_kinds::PEER_MERGED)
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let mut legacy = Legacy::new(&actor.earlier, &actor.messages);
    let mut queued = 0usize;
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
        if !delivered || merged.contains(&merged_message_id(&msg_id)) {
            continue;
        }
        let item = PeerItem::Message { msg_id, envelope };
        // 新写的 entry 没有事实就是没并入；旧的才按文字找（cross-session.md §2.2）。
        if !merge_tracked(&data) && legacy.message_shown(&item) {
            legacy.backfill.extend(item.merged_facts());
            continue;
        }
        actor.inbox.push_notice(Waiting::Peer(item));
        queued += 1;
    }
    // 没并入对话的回执：放回合并表，下一次醒来（立即到期）时交出。
    for (key, data) in receipts {
        actor.peers.seen.insert(key);
        let (Some(msg_id), Some(status), Some(recipient)) = (
            text_field(&data, "msgId"),
            text_field(&data, "status").and_then(|text| ReceiptStatus::parse(&text)),
            address(&data, "recipient"),
        ) else {
            continue;
        };
        if merged.contains(&merged_receipt_id(&msg_id, status)) {
            continue;
        }
        let receipt = PeerReceipt {
            seq: 0,
            msg_id,
            status,
            recipient,
        };
        if !merge_tracked(&data) && legacy.receipt_shown(&receipt) {
            legacy
                .backfill
                .push(merged_receipt(&receipt.msg_id, status));
            continue;
        }
        actor.peers.receipts.push(receipt);
        actor.peers.receipts_due = Some(actor.deps.clock.now_ms());
        queued += 1;
    }
    // 旧 entry 找到了文字：补记事实（一批），之后的冷加载不再找。失败时只记日志，下次打开再找一次。
    let backfill = legacy.backfill;
    let backfilled = backfill.len();
    if !backfill.is_empty()
        && let Err(error) = actor.commit(None, backfill).await
    {
        tracing::warn!(%error, "merged cross-session entries could not be recorded");
    }
    tracing::info!(
        event = "peers.restored",
        held = actor.peers.held.len(),
        queued,
        backfilled,
        "cross-session state restored"
    );
    // 打开时的模式可能已经不同；已过期的立即按过期结算（回执发给发送方的地址）。
    actor.peers_mode_changed(false).await;
    actor.peers_expire(false).await;
    if !actor.peers.held.is_empty() {
        actor.emit_held();
    }
}

#[cfg(test)]
#[path = "peer_merged_tests.rs"]
mod tests;
