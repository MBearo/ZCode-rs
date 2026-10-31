//! 本机跨会话通信的端口与公共类型（docs/specs/rust-cli/cross-session.md §2.3）。
//!
//! - [`PeerMessaging`]：进程级端口。本机实现（适配器 `rust-cli-peers`）管登记、收件箱 socket 与回执的发送；
//!   会话级的事实（收到的消息、扣住、解决、回执）只在会话 actor，先提交再 [`PeerMessaging::ack`]
//!   （README §6 规则 9）。
//! - 地址、`[ref]`、名字的规整与信封（`<cross-session-message …>`）的纯函数（cross-session.md §1）。给模型看的
//!   文字在 `rust-cli-prompts` 的 `peers` 模块。

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::SessionId;
use crate::permission::PermissionMode;

/// 信封的标签名。
pub const ENVELOPE_TAG: &str = "cross-session-message";
/// `[ref]` 的最短长度。
pub const REF_MIN: usize = 6;
/// `[ref]` 串的全长（sha256 hex 的前 12 位）。
pub const REF_MAX: usize = 12;
/// 扣住的消息最多几条；满了时最早的一条按过期丢弃。
pub const HELD_LIMIT: usize = 100;
/// 扣住消息的缺省过期时长。
pub const DEFAULT_HOLD_TIMEOUT_MS: u64 = 300_000;

/// 地址的前缀。
const ADDRESS_SCHEME: &str = "uds:";
/// 百分号编码的一个字节：`%` 加两位十六进制。
const PERCENT_ESCAPE_LEN: usize = 3;
const HEX_RADIX: u32 = rust_cli_base::units::HEX_RADIX;

/// 同伴的地址：`uds:<socket 路径>`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerAddress(String);

impl PeerAddress {
    /// socket 路径 → 地址。路径里 `[A-Za-z0-9:_/.\-]` 以外的字符按 UTF-8 百分号编码。
    pub fn from_socket(path: &str) -> Self {
        let mut encoded = String::with_capacity(path.len() + ADDRESS_SCHEME.len());
        encoded.push_str(ADDRESS_SCHEME);
        for character in path.chars() {
            if character.is_ascii_alphanumeric() || ":_/.\\-".contains(character) {
                encoded.push(character);
            } else {
                let mut buffer = [0u8; 4];
                for byte in character.encode_utf8(&mut buffer).bytes() {
                    encoded.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        Self(encoded)
    }

    /// 已经是地址形状的文本（`uds:` 开头）；其他形状为 None。
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(ADDRESS_SCHEME)?;
        (!rest.trim().is_empty()).then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// socket 路径（百分号解码）。
    pub fn socket(&self) -> String {
        percent_decode(self.0.strip_prefix(ADDRESS_SCHEME).unwrap_or(&self.0))
    }

    /// 12 位 ref 串：`sha256("session:" + socket 路径)` 的 hex 前 12 位。
    pub fn reference(&self) -> String {
        reference_of(&self.socket())
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(value) = text
                .get(index + 1..index + PERCENT_ESCAPE_LEN)
                .and_then(|hex| u8::from_str_radix(hex, HEX_RADIX).ok())
        {
            out.push(value);
            index += PERCENT_ESCAPE_LEN;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// socket 路径的 12 位 ref 串。
pub fn reference_of(socket: &str) -> String {
    let digest = Sha256::digest(format!("session:{socket}").as_bytes());
    let mut hex = String::with_capacity(REF_MAX);
    for byte in digest.iter().take(REF_MAX / 2) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// 每个候选显示的 ref：全部 12 位串排序，每项取 `max(6, 与相邻项的公共前缀长 + 1)`，不超过 12。
/// 返回与 `all` 同序的显示串。
pub fn shortest_refs(all: &[String]) -> Vec<String> {
    let mut sorted: Vec<&String> = all.iter().collect();
    sorted.sort();
    sorted.dedup();
    let common = |a: &str, b: &str| a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    all.iter()
        .map(|full| {
            let at = sorted.iter().position(|item| *item == full).unwrap_or(0);
            let before = at
                .checked_sub(1)
                .map_or(0, |prev| common(full, sorted[prev]));
            let after = sorted.get(at + 1).map_or(0, |next| common(full, next));
            let length = (before.max(after) + 1).max(REF_MIN).min(full.len());
            full[..length].to_owned()
        })
        .collect()
}

/// 名字的比较形式：NFKC 由调用方保证（名字是 ASCII 规整过的）；去掉控制字符、首尾空白，转小写，
/// 连续空白换成 `-`。
pub fn name_key(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|character| !character.is_control() || character.is_whitespace())
        .collect();
    cleaned
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// 目录名 → 会话名的前缀：按空白切词取前 4 个、转小写、非 `[a-z0-9]` 换成 `-`、截 40 字符、去掉首尾
/// 的 `-`；为空时是 `zcode`。
pub fn name_prefix(directory: &str) -> String {
    // 取前几个词、截到多少个字符。
    const WORDS: usize = 4;
    const MAX_CHARS: usize = 40;
    let words: Vec<&str> = directory.split_whitespace().take(WORDS).collect();
    let lower = words.join(" ").to_lowercase();
    let mut slug = String::new();
    let mut dash = false;
    for character in lower.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    let clipped: String = slug.chars().take(MAX_CHARS).collect();
    let trimmed = clipped.trim_matches('-');
    if trimmed.is_empty() {
        "zcode".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `name [ref]` 的拆分（`^(.*\S)\s*\[([0-9a-f]{6,12})\]$`）。
pub fn split_reference(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let inner = text.strip_suffix(']')?;
    let open = inner.rfind('[')?;
    let reference = &inner[open + 1..];
    let name = inner[..open].trim_end();
    let valid = (REF_MIN..=REF_MAX).contains(&reference.len())
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    (valid && !name.is_empty()).then_some((name, reference))
}

/// 权限模式的类别：`bypassPermissions` 是 bypass，其余都是 prompting。Node 的 guarded
/// 除危险命令外不询问，按 bypass 算。plan 开关不看：判断用会话的权限模式（plan 开着、可以 bypass 的交互
/// 会话里也算 bypass，与按底下的权限模式判断相同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeClass {
    Bypass,
    Prompting,
}

impl ModeClass {
    pub fn of(mode: PermissionMode) -> Self {
        if matches!(
            mode,
            PermissionMode::BypassPermissions | PermissionMode::Guarded
        ) {
            Self::Bypass
        } else {
            Self::Prompting
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bypass => "bypass",
            Self::Prompting => "prompting",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "bypass" => Some(Self::Bypass),
            "prompting" => Some(Self::Prompting),
            _ => None,
        }
    }
}

/// 扣住的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoldCause {
    ModeMismatch,
    NoModeAsserted,
}

/// 接收方对一条消息的判断（不含 `crossSessionInbound` 设置，cross-session.md §5 D6）。
pub fn gate(own: ModeClass, from: Option<ModeClass>) -> Option<HoldCause> {
    match from {
        Some(from) if from == own => None,
        Some(_) => Some(HoldCause::ModeMismatch),
        None if own == ModeClass::Bypass => Some(HoldCause::NoModeAsserted),
        None => None,
    }
}

/// 信封里的字段（解析结果的子集）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_mode: Option<ModeClass>,
    pub body: String,
}

/// 发送方包的信封：属性按 `from`、`from-name`、`from-mode` 的顺序；正文里 `</cross-session-message`
/// 的 `<` 换成 `<\`（只处理字面的情形，cross-session.md §5 D11）。
pub fn envelope(from: &PeerAddress, name: Option<&str>, mode: ModeClass, body: &str) -> String {
    let mut attributes = vec![format!("from=\"{}\"", from.as_str())];
    if let Some(name) = name
        .map(|name| name.replace(['"', '<', '>'], ""))
        .filter(|name| !name.trim().is_empty())
    {
        attributes.push(format!("from-name=\"{}\"", name.trim()));
    }
    attributes.push(format!("from-mode=\"{}\"", mode.as_str()));
    let closing = format!("</{ENVELOPE_TAG}");
    let neutral = body.replace(&closing, &format!("<\\/{ENVELOPE_TAG}"));
    format!(
        "<{ENVELOPE_TAG} {}>\n{neutral}\n</{ENVELOPE_TAG}>",
        attributes.join(" ")
    )
}

/// 解析信封：整段必须是一个信封；属性可以缺省。
pub fn parse_envelope(text: &str) -> Option<Envelope> {
    let rest = text.strip_prefix(&format!("<{ENVELOPE_TAG}"))?;
    let close = rest.find('>')?;
    let attributes = &rest[..close];
    let body = rest[close + 1..]
        .strip_prefix('\n')?
        .strip_suffix(&format!("\n</{ENVELOPE_TAG}>"))?;
    let attribute = |name: &str| {
        let key = format!(" {name}=\"");
        let start = attributes.find(&key)? + key.len();
        let end = attributes[start..].find('"')? + start;
        Some(attributes[start..end].to_owned())
    };
    Some(Envelope {
        from: attribute("from"),
        from_name: attribute("from-name"),
        from_mode: attribute("from-mode").and_then(|mode| ModeClass::parse(&mode)),
        body: body.to_owned(),
    })
}

/// 同伴会话的状态（登记里的 `status`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    Busy,
    Idle,
}

impl PeerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Idle => "idle",
        }
    }
}

/// 本进程自己的名字与地址（登记成功后才有）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerIdentity {
    pub name: String,
    pub address: PeerAddress,
}

/// 一个活着的同伴会话（读登记处得到的快照，不含自己）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    pub pid: u32,
    pub name: String,
    pub address: PeerAddress,
    /// 登记的 `kind`（交互式与 `-p` 都是 `interactive`）。
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PeerStatus>,
    pub started_at_ms: u64,
}

/// 发出的一条消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerOutbound {
    pub to: PeerAddress,
    /// 不带前缀的 uuid。
    pub msg_id: String,
    /// 收件方显示用的名字（发送方解析出的名字）。
    pub recipient_name: String,
    /// 完整的信封（[`envelope`]）。
    pub envelope: String,
}

/// 收到的一条消息（还没提交）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundPeerMessage {
    /// 在接收会话内单调递增；ack 用它。
    pub seq: u64,
    pub msg_id: String,
    pub from: PeerAddress,
    /// 信封原文；名字与模式类别从它解析（[`parse_envelope`]）。
    pub envelope: String,
    pub received_at_ms: u64,
}

/// 回执的状态（`peer_message_status` 的 `status`；`dropped` 不发，cross-session.md §5 D2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Held,
    Delivered,
    Denied,
    Expired,
    Refused,
}

impl ReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Delivered => "delivered",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Refused => "refused",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "held" => Some(Self::Held),
            "delivered" => Some(Self::Delivered),
            "denied" => Some(Self::Denied),
            "expired" => Some(Self::Expired),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }
}

/// 发送方收到的一条回执（还没提交）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerReceipt {
    pub seq: u64,
    /// 发出的那条消息的 msg_id。
    pub msg_id: String,
    pub status: ReceiptStatus,
    /// 收件方的地址（回执文字的 `recipient:`）。
    pub recipient: PeerAddress,
}

/// 适配器交给会话 actor 的入站事实（至少一次：提交后 ack，没 ack 的再送）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerSignal {
    Message(InboundPeerMessage),
    Receipt(PeerReceipt),
}

impl PeerSignal {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Message(message) => message.seq,
            Self::Receipt(receipt) => receipt.seq,
        }
    }
}

/// 一条扣住的消息（[`crate::session_api::SessionView::peer_held`]，界面的批准框只从这里取）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldPeerMessage {
    pub msg_id: String,
    pub from: PeerAddress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// 信封里的正文（批准时整段投递）。
    pub body: String,
    pub cause: HoldCause,
    pub received_at_ms: u64,
    /// 到这一刻还没解决就按过期丢弃（数据，到点由时钟判断）。
    pub expires_at_ms: u64,
}

/// 发送失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerSendError {
    /// 连不上（对方已退出、socket 不在）。
    #[error("the peer session is gone")]
    Gone,
    /// 对方拒收（例如没有绑定的会话，或正在关闭）。
    #[error("refused: {reason}")]
    Refused { reason: String },
    #[error("{message}")]
    Failed { message: String },
}

/// 进程级的跨会话端口（本机实现见适配器 `rust-cli-peers`）。
#[async_trait]
pub trait PeerMessaging: Send + Sync {
    /// 本进程的名字与地址；跨会话关闭（绑定失败、不支持的平台）时为 None。
    async fn identity(&self) -> Option<PeerIdentity>;

    /// 活着的同伴（读登记处，顺手清掉 pid 已不存在的记录），不含自己。
    async fn peers(&self) -> Vec<PeerRecord>;

    /// 发一条消息，等对方会话提交后回复（cross-session.md §2.1）。
    async fn send(&self, message: PeerOutbound) -> Result<(), PeerSendError>;

    /// 发回执（适配器自己的任务里发，不等；失败只记日志）。
    async fn receipt(&self, to: PeerAddress, msg_id: String, status: ReceiptStatus);

    /// 顶层会话打开：成为本进程的主会话（登记的 `sessionId` 跟着改），之后的入站交给它。
    async fn bind(&self, session: &SessionId);

    /// 会话关闭：不再是主会话；没 ack 的入站留给下一个绑定的会话。
    async fn unbind(&self, session: &SessionId);

    /// 这个会话的入站流（绑定之前订阅也可以，绑定后才有内容）。
    fn signals(&self, session: &SessionId) -> BoxStream<'static, PeerSignal>;

    /// 入站已提交。
    async fn ack(&self, session: &SessionId, seq: u64);

    /// 登记里的状态（回合开始 busy，空闲 idle）。
    async fn set_status(&self, session: &SessionId, status: PeerStatus);
}

/// 跨会话关闭时的空实现。
pub struct NoPeers;

#[async_trait]
impl PeerMessaging for NoPeers {
    async fn identity(&self) -> Option<PeerIdentity> {
        None
    }

    async fn peers(&self) -> Vec<PeerRecord> {
        Vec::new()
    }

    async fn send(&self, _message: PeerOutbound) -> Result<(), PeerSendError> {
        Err(PeerSendError::Gone)
    }

    async fn receipt(&self, _to: PeerAddress, _msg_id: String, _status: ReceiptStatus) {}

    async fn bind(&self, _session: &SessionId) {}

    async fn unbind(&self, _session: &SessionId) {}

    fn signals(&self, _session: &SessionId) -> BoxStream<'static, PeerSignal> {
        Box::pin(futures_util::stream::pending())
    }

    async fn ack(&self, _session: &SessionId, _seq: u64) {}

    async fn set_status(&self, _session: &SessionId, _status: PeerStatus) {}
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
