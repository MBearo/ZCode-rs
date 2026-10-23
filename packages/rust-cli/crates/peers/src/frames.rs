//! 线上的帧与登记的记录（docs/specs/rust-cli/cross-session.md §1.5、§2.2）：NDJSON，一个连接一条。
//! 消息是 `type: user`，回执是 `type: control` / `action: peer_message_status`，另加
//! 本工程的回复行（接收方的会话提交之后才回 `accepted`，README §6 规则 9）。

use serde::{Deserialize, Serialize};

/// 登记的版本。
pub(crate) const PEER_PROTOCOL: u32 = 1;

/// `<pid>.json` 的内容（字段与键序固定）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub cwd: String,
    pub started_at: u64,
    pub version: String,
    pub peer_protocol: u32,
    pub kind: String,
    pub messaging_socket_path: String,
    pub name: String,
    #[serde(default)]
    pub name_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_updated_at: Option<u64>,
}

/// `type: user` 的 `message`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UserBody {
    pub role: String,
    pub content: String,
}

/// 收件箱收到的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Frame {
    User {
        msg_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        message: UserBody,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<String>,
    },
    Control {
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        orig_msg_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
}

/// 接收方的回复行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Reply {
    /// 接收方的会话已提交（消息或回执）。
    Accepted,
    /// 不收：没有绑定的会话且正在退出、帧不对。
    Refused { reason: String },
    /// 收到了但不处理（不认识的回执）。
    Ignored,
}

pub(crate) fn line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
