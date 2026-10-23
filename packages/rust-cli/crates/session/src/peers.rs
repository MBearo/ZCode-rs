//! 跨会话的入站、扣住、解决与回执（docs/specs/rust-cli/cross-session.md §2.4）。
//!
//! 会话 actor 是收到的消息与回执的唯一 owner：每一条先提交 `rust-cli/peer_*` entry 再 ack 适配器（适配器这时才回复
//! 对方，README §6 规则 9）。接收的消息与回执进 next 通道（lane.rs），运行中在工具边界并入、空闲时开回合；扣住的
//! 消息在 `held` 表里，到期时刻是数据，`select!` 里睡到最早的到期时刻、醒来后按注入的时钟判断（cron 的唤醒同理）。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::peers::{
    HELD_LIMIT, HeldPeerMessage, HoldCause, InboundPeerMessage, ModeClass, PeerAddress,
    PeerMessaging, PeerReceipt, PeerSignal, PeerStatus, ReceiptStatus, gate, parse_envelope,
};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::store::{Fact, SessionKind};
use serde_json::{Value, json};

use crate::actor::Actor;
use crate::lane::{Lane, Waiting};

/// 收到的消息、扣住的解决与回执的 entry 种类（node-store 映射成 `rust-cli/<kind>`）。
pub(crate) const MESSAGE_ENTRY: &str = "peer_message";
pub(crate) const RESOLVED_ENTRY: &str = "peer_resolved";
pub(crate) const RECEIPT_ENTRY: &str = "peer_receipt";
/// 开回合的 meta user 消息的来源。
pub(crate) const PEER_SOURCE: &str = "cross-session";

/// 会话 actor 用到的跨会话能力（cross-session.md §2.4）。
#[derive(Clone)]
pub struct PeerDeps {
    pub port: Arc<dyn PeerMessaging>,
    /// 扣住的消息多久过期（入口读 `ZCODE_USER_DIALOG_TIMEOUT_MS`，缺省 300000 ms）。
    pub hold_timeout_ms: u64,
    /// 回执在空闲时开回合（交互式，优先级 `later`）；`-p` 为 false，只在工具边界并入（passive）。
    pub receipts_wake: bool,
}

impl PeerDeps {
    /// 跨会话关闭（测试、子代理之外的嵌入）。
    pub fn off() -> Self {
        Self {
            port: Arc::new(rust_cli_kernel_api::peers::NoPeers),
            hold_timeout_ms: rust_cli_kernel_api::peers::DEFAULT_HOLD_TIMEOUT_MS,
            receipts_wake: true,
        }
    }
}

/// next 通道里的一项同伴输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PeerItem {
    /// 接收的消息（信封原文）。
    Message { msg_id: String, envelope: String },
    /// 回执（全文，已加前缀）；`passive` 时只在工具边界并入，不开回合。
    Receipt {
        key: String,
        text: String,
        passive: bool,
    },
}

impl PeerItem {
    pub(crate) fn key(&self) -> &str {
        match self {
            Self::Message { msg_id, .. } => msg_id,
            Self::Receipt { key, .. } => key,
        }
    }

    /// 工具边界并入的文字（回执两种场合相同）。
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Message { envelope, .. } => rust_cli_prompts::peers::wrap(envelope, true),
            Self::Receipt { text, .. } => text.clone(),
        }
    }

    /// 空闲时开回合的文字。
    pub(crate) fn turn_text(&self) -> String {
        match self {
            Self::Message { envelope, .. } => rust_cli_prompts::peers::wrap(envelope, false),
            Self::Receipt { text, .. } => text.clone(),
        }
    }

    pub(crate) fn passive(&self) -> bool {
        matches!(self, Self::Receipt { passive: true, .. })
    }
}

/// 扣住的一条：界面看到的快照与批准时投递的信封。
#[derive(Debug, Clone)]
pub(crate) struct Held {
    pub(crate) view: HeldPeerMessage,
    pub(crate) envelope: String,
}

/// actor 里的跨会话状态。
#[derive(Default)]
pub(crate) struct PeerState {
    pub(crate) held: Vec<Held>,
    /// 已提交过的 msg_id 与回执键：重复送达只 ack（至少一次）。
    pub(crate) seen: HashSet<String>,
    /// 登记里最后写的状态（只在变化时写）。
    pub(crate) status: Option<PeerStatus>,
    /// 还没交出的回执与交出的时刻（500 ms 合并，peer_receipts.rs）。
    pub(crate) receipts: Vec<PeerReceipt>,
    pub(crate) receipts_due: Option<u64>,
}

impl PeerState {
    pub(crate) fn views(&self) -> Vec<HeldPeerMessage> {
        self.held.iter().map(|held| held.view.clone()).collect()
    }

    /// 最早的到期时刻：扣住的消息过期、合并中的回执交出。
    fn earliest(&self) -> Option<u64> {
        self.held
            .iter()
            .map(|held| held.view.expires_at_ms)
            .filter(|at| *at != u64::MAX)
            .chain(self.receipts_due)
            .min()
    }
}

/// 睡到最早的到期时刻（没有扣住的消息与合并中的回执时一直等）。醒来不等于到期：由调用方按时钟判断
/// （cross-session.md §2.4）。
pub(crate) async fn deadline(state: &PeerState, now_ms: u64) {
    match state.earliest() {
        Some(at) => tokio::time::sleep(Duration::from_millis(at.saturating_sub(now_ms))).await,
        None => std::future::pending().await,
    }
}

fn receipt_status(outcome: &str) -> Option<ReceiptStatus> {
    ReceiptStatus::parse(outcome)
}

fn cause_name(cause: HoldCause) -> &'static str {
    match cause {
        HoldCause::ModeMismatch => "mode-mismatch",
        HoldCause::NoModeAsserted => "no-mode-asserted",
    }
}

impl Actor {
    fn peer_mode(&self) -> ModeClass {
        ModeClass::of(self.execution.mode)
    }

    /// 顶层会话才收跨会话的消息（子代理的子会话不绑定）。
    fn peer_bound(&self) -> bool {
        self.meta.kind != SessionKind::Subagent
    }

    /// 打开会话：绑定为本进程的主会话，再从库里恢复扣住的消息、没并入的消息与回执（cross-session.md §2.2）。
    /// 恢复不开回合：等下一个回合或工具边界取用。
    pub(crate) async fn peers_open(&mut self) {
        if !self.peer_bound() {
            return;
        }
        self.deps.peers.port.bind(&self.meta.id).await;
        crate::peer_restore::restore(self).await;
        self.peer_status(if self.busy() {
            PeerStatus::Busy
        } else {
            PeerStatus::Idle
        })
        .await;
    }

    pub(crate) async fn peers_close(&self) {
        if self.peer_bound() {
            self.deps.peers.port.unbind(&self.meta.id).await;
        }
    }

    pub(crate) async fn peer_status(&mut self, status: PeerStatus) {
        if !self.peer_bound() || self.peers.status == Some(status) {
            return;
        }
        self.peers.status = Some(status);
        self.deps.peers.port.set_status(&self.meta.id, status).await;
    }

    pub(crate) async fn on_peer_signal(&mut self, signal: PeerSignal) {
        // 关闭中不收（R1-1）：不 ack，适配器把它交给下一个绑定的会话。
        if self.closing.is_some() {
            return;
        }
        match signal {
            PeerSignal::Message(message) => self.on_peer_message(message).await,
            PeerSignal::Receipt(receipt) => self.on_peer_receipt(receipt).await,
        }
    }

    async fn on_peer_message(&mut self, message: InboundPeerMessage) {
        if self.peers.seen.contains(&message.msg_id) {
            self.deps.peers.port.ack(&self.meta.id, message.seq).await;
            return;
        }
        let envelope = parse_envelope(&message.envelope);
        let from_mode = envelope.as_ref().and_then(|envelope| envelope.from_mode);
        let cause = gate(self.peer_mode(), from_mode);
        // 超时为 0（`never`）时不计时：扣住的消息不过期，只等批准或改模式。
        let expires_at_ms = match self.deps.peers.hold_timeout_ms {
            0 => u64::MAX,
            timeout => message.received_at_ms.saturating_add(timeout),
        };
        let data = json!({
            "msgId": message.msg_id,
            "from": message.from.as_str(),
            "fromName": envelope.as_ref().and_then(|envelope| envelope.from_name.clone()),
            "fromMode": from_mode.map(ModeClass::as_str),
            "envelope": message.envelope,
            "receivedAt": message.received_at_ms,
            "held": cause.map(|cause| json!({"cause": cause_name(cause), "expiresAt": expires_at_ms})),
        });
        let fact = Fact::Entry {
            kind: MESSAGE_ENTRY.to_owned(),
            id: message.msg_id.clone(),
            data,
        };
        if let Err(reason) = self.commit(None, vec![fact]).await {
            // 没提交就不 ack：对方的连接一直等，适配器之后再送。
            tracing::warn!(%reason, "a cross-session message could not be stored");
            return;
        }
        self.peers.seen.insert(message.msg_id.clone());
        self.deps.peers.port.ack(&self.meta.id, message.seq).await;
        tracing::info!(
            event = "peers.received",
            held = cause.is_some(),
            "cross-session message received"
        );
        match cause {
            None => {
                self.lane
                    .push_back(Lane::Notice(Waiting::Peer(PeerItem::Message {
                        msg_id: message.msg_id,
                        envelope: message.envelope,
                    })));
                if self.running.is_none() {
                    self.start_next().await;
                }
            }
            Some(cause) => {
                let held = Held {
                    view: HeldPeerMessage {
                        msg_id: message.msg_id.clone(),
                        from: message.from.clone(),
                        from_name: envelope
                            .as_ref()
                            .and_then(|envelope| envelope.from_name.clone()),
                        body: envelope.map(|envelope| envelope.body).unwrap_or_default(),
                        cause,
                        received_at_ms: message.received_at_ms,
                        expires_at_ms,
                    },
                    envelope: message.envelope,
                };
                self.hold(held).await;
                self.deps
                    .peers
                    .port
                    .receipt(message.from, message.msg_id, ReceiptStatus::Held)
                    .await;
            }
        }
    }

    /// 放进扣住表；满了（100 条）时最早的一条按过期丢弃。
    pub(crate) async fn hold(&mut self, held: Held) {
        if self.peers.held.len() >= HELD_LIMIT {
            let oldest = self.peers.held[0].view.msg_id.clone();
            self.resolve_held(&oldest, ReceiptStatus::Expired).await;
        }
        self.peers.held.push(held);
        self.emit_held();
    }

    pub(crate) fn emit_held(&self) {
        self.log.append(KernelEvent::PeerHeldChanged {
            held: self.peers.views(),
        });
    }

    /// 解决一条扣住的消息：先提交 `peer_resolved`，再放行（delivered）或丢弃，最后回执。已经不在表里时为 false。
    pub(crate) async fn resolve_held(&mut self, msg_id: &str, outcome: ReceiptStatus) -> bool {
        let Some(at) = self
            .peers
            .held
            .iter()
            .position(|held| held.view.msg_id == msg_id)
        else {
            return false;
        };
        let fact = Fact::Entry {
            kind: RESOLVED_ENTRY.to_owned(),
            id: msg_id.to_owned(),
            data: json!({"msgId": msg_id, "outcome": outcome.as_str()}),
        };
        if let Err(reason) = self.commit(None, vec![fact]).await {
            tracing::warn!(%reason, "a held cross-session message could not be resolved");
            return false;
        }
        let held = self.peers.held.remove(at);
        tracing::info!(
            event = "peers.resolved",
            outcome = outcome.as_str(),
            "held cross-session message resolved"
        );
        if outcome == ReceiptStatus::Delivered {
            self.lane
                .push_back(Lane::Notice(Waiting::Peer(PeerItem::Message {
                    msg_id: held.view.msg_id.clone(),
                    envelope: held.envelope,
                })));
        }
        self.emit_held();
        self.deps
            .peers
            .port
            .receipt(held.view.from, held.view.msg_id, outcome)
            .await;
        true
    }

    /// 前端批准或拒绝（`Command::ResolvePeerMessage`）。批准且空闲时开回合（放行进队列）。
    pub(crate) async fn resolve_peer(&mut self, msg_id: &str, approve: bool) -> Result<(), String> {
        let outcome = if approve {
            ReceiptStatus::Delivered
        } else {
            ReceiptStatus::Denied
        };
        if !self.resolve_held(msg_id, outcome).await {
            return Err("that held message was already resolved".to_owned());
        }
        if approve && self.running.is_none() {
            self.start_next().await;
        }
        Ok(())
    }

    /// 权限模式变了（mode.rs 的唯一入口）：现在能接收的扣住消息放行（`mode-changed`）。
    pub(crate) async fn peers_mode_changed(&mut self, start: bool) {
        let own = self.peer_mode();
        let released: Vec<String> = self
            .peers
            .held
            .iter()
            .filter(|held| {
                let from = parse_envelope(&held.envelope).and_then(|envelope| envelope.from_mode);
                gate(own, from).is_none()
            })
            .map(|held| held.view.msg_id.clone())
            .collect();
        for msg_id in &released {
            self.resolve_held(msg_id, ReceiptStatus::Delivered).await;
        }
        if start && !released.is_empty() && self.running.is_none() {
            self.start_next().await;
        }
    }

    /// 醒来：合并到期的回执交出，到期的扣住消息按过期丢弃（都按时钟判断，cross-session.md §2.4）。`start`：交出的
    /// 回执可以开回合（重开会话的恢复不开）。
    pub(crate) async fn peers_expire(&mut self, start: bool) {
        let now = self.deps.clock.now_ms();
        if self.peers.receipts_due.is_some_and(|due| due <= now) {
            self.flush_receipts(start).await;
        }
        let due: Vec<String> = self
            .peers
            .held
            .iter()
            .filter(|held| held.view.expires_at_ms <= now)
            .map(|held| held.view.msg_id.clone())
            .collect();
        for msg_id in due {
            self.resolve_held(&msg_id, ReceiptStatus::Expired).await;
        }
    }
}

/// 冷加载用：entry 的 data 里的字段。
pub(crate) fn text_field(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn outcome_of(data: &Value) -> Option<ReceiptStatus> {
    data.get("outcome")
        .and_then(Value::as_str)
        .and_then(receipt_status)
}

pub(crate) fn address(data: &Value, key: &str) -> Option<PeerAddress> {
    text_field(data, key).and_then(|text| PeerAddress::parse(&text))
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
