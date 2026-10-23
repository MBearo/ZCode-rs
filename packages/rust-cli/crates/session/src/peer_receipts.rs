//! 发送方收到的回执（docs/specs/rust-cli/cross-session.md §1.9、§2.4）：先提交 `rust-cli/peer_receipt` 再 ack；同一种状态
//! 在最后一条到达后 500 ms 内的合成一条，到点（按时钟）才交给 next 通道，所以刚发出就回来的
//! held 不会挤进 SendMessage 自己的工具边界。交出时每种状态一条，顺序是 held、
//! denied、expired、delivered、refused。

use rust_cli_kernel_api::peers::{PeerReceipt, ReceiptStatus};
use rust_cli_kernel_api::store::Fact;
use serde_json::json;

use crate::actor::Actor;
use crate::lane::{Lane, Waiting};
use crate::peers::{PeerItem, RECEIPT_ENTRY};

/// 最后一条回执之后等这么久再交出。
pub(crate) const RECEIPT_WINDOW_MS: u64 = 500;

/// 交出的顺序。
const ORDER: [ReceiptStatus; 5] = [
    ReceiptStatus::Held,
    ReceiptStatus::Denied,
    ReceiptStatus::Expired,
    ReceiptStatus::Delivered,
    ReceiptStatus::Refused,
];

/// 几条同一状态的回执的全文（带前缀）。
fn group_text(status: ReceiptStatus, receipts: &[&PeerReceipt]) -> String {
    let recipients: Vec<&str> = receipts.iter().map(|r| r.recipient.as_str()).collect();
    let msg_ids: Vec<&str> = receipts.iter().map(|r| r.msg_id.as_str()).collect();
    let body = rust_cli_prompts::peers::receipt(status.as_str(), &recipients, &msg_ids)
        .unwrap_or_default();
    rust_cli_prompts::peers::non_user(&body)
}

impl Actor {
    pub(crate) async fn on_peer_receipt(&mut self, receipt: PeerReceipt) {
        let key = format!("{}:{}", receipt.msg_id, receipt.status.as_str());
        if self.peers.seen.contains(&key) {
            self.deps.peers.port.ack(&self.meta.id, receipt.seq).await;
            return;
        }
        let fact = Fact::Entry {
            kind: RECEIPT_ENTRY.to_owned(),
            id: key.clone(),
            data: json!({
                "msgId": receipt.msg_id,
                "status": receipt.status.as_str(),
                "recipient": receipt.recipient.as_str(),
            }),
        };
        if let Err(reason) = self.commit(None, vec![fact]).await {
            tracing::warn!(%reason, "a cross-session receipt could not be stored");
            return;
        }
        self.peers.seen.insert(key);
        self.deps.peers.port.ack(&self.meta.id, receipt.seq).await;
        self.peers.receipts.push(receipt);
        self.peers.receipts_due = Some(self.deps.clock.now_ms() + RECEIPT_WINDOW_MS);
    }

    /// 合并窗口到了：每种状态一条进 next 通道；交互式空闲时开回合，`-p` 只在工具边界并入（passive）。
    pub(crate) async fn flush_receipts(&mut self, start: bool) {
        self.peers.receipts_due = None;
        let pending = std::mem::take(&mut self.peers.receipts);
        if pending.is_empty() {
            return;
        }
        let passive = !self.deps.peers.receipts_wake;
        for status in ORDER {
            let group: Vec<&PeerReceipt> = pending.iter().filter(|r| r.status == status).collect();
            let Some(first) = group.first() else { continue };
            let key = format!("{}:{}", first.msg_id, status.as_str());
            self.lane
                .push_back(Lane::Notice(Waiting::Peer(PeerItem::Receipt {
                    key,
                    text: group_text(status, &group),
                    passive,
                })));
        }
        if start && self.running.is_none() && !passive {
            self.start_next().await;
        }
    }
}
