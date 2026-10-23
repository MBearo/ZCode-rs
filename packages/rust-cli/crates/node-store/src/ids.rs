// 复制自 apps/zcode-cli-rust/crates/domain/src/node_ids.rs（feat/rs 79b6691c55）。
//! Node 会话库的 id 与 slug 格式（Node `contracts/src/interfaces/shared.ts`、
//! `core/src/runtime/helpers/project.ts`，docs/specs/rust-m11-node-storage.md §2.3）。
//!
//! 与复制来源的差异：只保留本 crate 用到的格式；新增 [`message_time`]，从消息 id 取创建时间。
//!
//! F7（审查 R1-15、R2-11）：Node 的 ID 格式只在这里。[`NodeIds`] 从 runtime 移来，内核（`Ports.ids`）与
//! 本 crate 写记录（part、fork 出的消息、turn、artifact）用同一个实例；它是全工程唯一生成随机 uuid 的地方
//! （[`RandomUuids`]）。以前 runtime 与本 crate 各有一份 36 进制、turn id 与随机源（`RecordSource`）。

use std::sync::Arc;

use rust_cli_base::js::base36;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::ids::{
    AgentId, CommandId, Generation, InteractionId, MessageId, OperationId, RunId, SessionId, TurnId,
};

/// 随机 uuid 的来源：生产是 [`RandomUuids`]，测试注入顺序值与 Node 夹具逐字节比对。
pub trait UuidSource: Send + Sync {
    fn uuid(&self) -> String;
}

/// uuid v4。
pub struct RandomUuids;

impl UuidSource for RandomUuids {
    #[expect(
        clippy::disallowed_methods,
        reason = "NodeIds 的随机源是全工程唯一生成 uuid 的地方（README §4 的 ID 类入口）"
    )]
    fn uuid(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Node 格式的 ID：`sess_<uuid>`、`msg_<36 进制毫秒>_<uuid>`、`turn_<uuid>`、`agent_<uuid>` 与
/// `sess_subagent_agent_<uuid>`。Node 没有的 ID（run、operation、interaction）用 `<前缀>_<uuid>`；命令与执行环境
/// 实例是不带前缀的 uuid。带着注入的时钟：消息 ID 里的毫秒与记录的时间同源。
pub struct NodeIds {
    clock: Arc<dyn Clock>,
    uuids: Arc<dyn UuidSource>,
}

impl NodeIds {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_uuids(clock, Arc::new(RandomUuids))
    }

    pub fn with_uuids(clock: Arc<dyn Clock>, uuids: Arc<dyn UuidSource>) -> Self {
        Self { clock, uuids }
    }

    /// 注入的时钟的当前时间（记录的 `time_*`）。
    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// 一个新的 uuid（part、fork 出的消息、artifact 的 ID 里的随机部分；外部编辑器的临时文件名）。
    pub fn uuid(&self) -> String {
        self.uuids.uuid()
    }
}

impl Ids for NodeIds {
    fn session(&self) -> SessionId {
        SessionId::new(format!("sess_{}", self.uuid()))
    }

    fn subagent(&self) -> (AgentId, SessionId) {
        let id = self.uuid();
        (
            AgentId::new(format!("agent_{id}")),
            SessionId::new(format!("sess_subagent_agent_{id}")),
        )
    }

    fn message(&self) -> MessageId {
        MessageId::new(message_id(self.now_ms(), &self.uuid()))
    }

    fn turn(&self) -> TurnId {
        TurnId::new(turn_id(&self.uuid()))
    }

    fn run(&self) -> RunId {
        RunId::new(format!("run_{}", self.uuid()))
    }

    fn operation(&self) -> OperationId {
        OperationId::new(format!("op_{}", self.uuid()))
    }

    fn interaction(&self) -> InteractionId {
        InteractionId::new(format!("interaction_{}", self.uuid()))
    }

    fn command(&self) -> CommandId {
        CommandId::new(self.uuid())
    }

    fn generation(&self) -> Generation {
        Generation::new(self.uuid())
    }
}

/// Node `createMessageId`：`msg_<毫秒的 36 进制>_<uuid>`。
pub fn message_id(now_ms: u64, uuid: &str) -> String {
    format!("msg_{}_{uuid}", base36(now_ms))
}

/// Node `createPartId`：`part_<毫秒的 36 进制>_<uuid>`。
pub fn part_id(now_ms: u64, uuid: &str) -> String {
    format!("part_{}_{uuid}", base36(now_ms))
}

/// Node `createTurnId`。
pub fn turn_id(uuid: &str) -> String {
    format!("turn_{uuid}")
}

/// `msg_<毫秒的 36 进制>_<uuid>`（Node `createMessageId`）里的毫秒；格式不符时为 `None`。
pub fn message_time(id: &str) -> Option<u64> {
    let digits = id.strip_prefix("msg_")?.split_once('_')?.0;
    if digits.is_empty() || digits.len() > 11 {
        return None;
    }
    u64::from_str_radix(digits, 36).ok()
}

/// 消息的创建时间：内核的消息 id 是 `msg_<创建毫秒>_<uuid>`（与 Node 相同），取其中的毫秒；
/// 格式不符或晚于提交时间（时钟回拨）时取提交时间。
pub fn created_at(id: &str, now: u64) -> u64 {
    message_time(id).filter(|at| *at <= now).unwrap_or(now)
}

/// `[^a-z0-9._-]+` 换成 `-`，再去掉首尾的 `-`。
fn dashed(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut dash = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

/// Node core `slugify`：结果为空时是 `session`。
pub fn slugify(value: &str) -> String {
    let slug = dashed(value);
    if slug.is_empty() {
        "session".into()
    } else {
        slug
    }
}

/// Node core `projectIdFromDirectory`：`proj_` 加 slug 的前 80 个字符（slug 只有 ASCII，按字节截取
/// 等于 JS `slice`）。
pub fn project_id(directory: &str) -> String {
    let slug = slugify(directory);
    format!("proj_{}", &slug[..slug.len().min(80)])
}

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
