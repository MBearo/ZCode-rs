//! `v4/command`（docs/specs/rust-cli/app-protocol.md §6.4）：信封与 payload 过 schema、按 (sessionId, commandId) 幂等、
//! 转交内核、回 ACK；`v4/commands/query` 查 ACK。P1 实现 createSession、sendText、stop、resolveInteraction 与
//! setFollowupMode，其余命令回 rejected（`proto.unsupportedCommand`）。
//!
//! revision 只来自会话 actor（G1）：CAS 命令（P1 只有 setFollowupMode）在裁决时向 actor 取当前的 revision 比较。
//! 命令事实（G2）：会话命令的 accepted / rejected / failed 照 Node 的 `v4/command_fact` 同格式落库，回退到 Node 时它读得到。

use std::collections::{HashMap, VecDeque};

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::FrontendRecord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 命令事实的记录类型与 id（Node `persistent-command-facts.ts`）。
pub const COMMAND_FACT: &str = "v4/command_fact";
const FACT_SOURCE: &str = "timeline";
/// 幂等表的上限（全进程；Node 每会话 512）。
const REMEMBERED: usize = 4096;

/// 命令信封（`commandEnvelopeSchema`；已经过 schema 校验，这里只取用到的字段）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub command_id: String,
    pub client_id: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub base_revision: Option<u64>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub input_origin: Option<String>,
}

/// ACK 的状态（`commandAckSchema.status`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Accepted,
    Rejected,
    Stale,
    Duplicate,
    Noop,
    Failed,
}

/// 命令的 ACK（`commandAckSchema`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ack {
    pub command_id: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub revision_at_decision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub memory_enabled: bool,
}

impl Ack {
    pub fn accepted(command: &str, revision: u64, result: Option<Value>) -> Self {
        Self {
            command_id: command.to_owned(),
            status: Status::Accepted,
            reason_code: None,
            message: None,
            revision_at_decision: revision,
            result,
            memory_enabled: false,
        }
    }

    pub fn refused(
        command: &str,
        status: Status,
        reason: &str,
        message: impl Into<String>,
        revision: u64,
    ) -> Self {
        Self {
            command_id: command.to_owned(),
            status,
            reason_code: Some(reason.to_owned()),
            message: Some(message.into()),
            revision_at_decision: revision,
            result: None,
            memory_enabled: false,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// 会落库的状态（Node 的 `commandAck` 只认这三种）。
    pub fn is_fact(&self) -> bool {
        matches!(
            self.status,
            Status::Accepted | Status::Rejected | Status::Failed
        )
    }
}

/// 一个命令的键：createSession 在全局桶（sessionId 为 null）。
pub fn key(session: Option<&str>, command: &str) -> (String, String) {
    (session.unwrap_or_default().to_owned(), command.to_owned())
}

/// 内存里的幂等表（先进先出地淘汰）。
#[derive(Default)]
pub struct Remembered {
    acks: HashMap<(String, String), Ack>,
    order: VecDeque<(String, String)>,
}

impl Remembered {
    pub fn get(&self, key: &(String, String)) -> Option<&Ack> {
        self.acks.get(key)
    }

    pub fn put(&mut self, key: (String, String), ack: Ack) {
        if self.acks.insert(key.clone(), ack).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > REMEMBERED {
            if let Some(oldest) = self.order.pop_front() {
                self.acks.remove(&oldest);
            }
        }
    }
}

/// 重复的命令：原来的 ACK，状态改成 duplicate。
pub fn duplicate(original: &Ack) -> Ack {
    Ack {
        status: Status::Duplicate,
        ..original.clone()
    }
}

/// 命令事实的记录（Node `savePersistentCommandFact` 同格式）。
pub fn fact(ack: &Ack) -> FrontendRecord {
    FrontendRecord {
        kind: COMMAND_FACT.to_owned(),
        key: format!("v4_command_fact:{FACT_SOURCE}:{}", ack.command_id),
        data: json!({"source": FACT_SOURCE, "ack": ack.to_value()}),
    }
}

/// 从落库的命令事实里找一个命令的 ACK（Node 也写 `child` 来源）。
pub fn find_fact(records: &[FrontendRecord], command: &str) -> Option<Ack> {
    records.iter().find_map(|record| {
        let source = record.data["source"].as_str()?;
        if source != "timeline" && source != "child" {
            return None;
        }
        let ack: Ack = serde_json::from_value(record.data["ack"].clone()).ok()?;
        (ack.command_id == command).then_some(ack)
    })
}

/// 生成会话 ID（Host 没给时；Node 的写法 `session_<32 位十六进制>`）。
pub fn new_session_id() -> SessionId {
    SessionId::new(format!(
        "session_{:016x}{:016x}",
        rust_cli_base::random::u64(),
        rust_cli_base::random::u64()
    ))
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
