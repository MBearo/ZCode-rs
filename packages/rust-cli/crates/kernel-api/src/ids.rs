//! 类型化 ID（docs/specs/rust-cli/README.md §8）。
//!
//! 每种 ID 是独立的 newtype，不能互相混用。ID 的生成由 [`crate::clock::Ids`] 负责，格式与 Node
//! 记录一致（例如会话 `sess_<uuid>`、消息 `msg_<36 进制毫秒>_<uuid>`），所以内核里的 ID 就是落库
//! 的 ID，不需要映射。

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// 会话（Node `sess_<uuid>`；子代理 `sess_subagent_agent_<uuid>`）。
    SessionId
);
string_id!(
    /// 一条消息（Node `msg_<36 进制毫秒>_<uuid>`）。
    MessageId
);
string_id!(
    /// 一轮对话（Node `turn_<uuid>`）。
    TurnId
);
string_id!(
    /// 模型给出的 tool_use id（`toolu_…`），由模型决定，内核原样保留。
    ToolUseId
);
string_id!(
    /// 一次有副作用的执行操作；内核生成，先落库再调执行环境（§6 规则 3）。
    OperationId
);
string_id!(
    /// 前端发来的一条命令；同一个 ID 重复提交时幂等。
    CommandId
);
string_id!(
    /// 子代理（Node `agent_<uuid>`）。
    AgentId
);
string_id!(
    /// 一个执行环境（本地是本机，云端是账号的执行环境）。
    EnvironmentId
);
string_id!(
    /// 执行环境的当前实例；实例被替换时变化，旧实例的句柄随之失效。
    Generation
);
string_id!(
    /// 一次运行（一轮对话内模型与工具的执行链）。
    RunId
);
string_id!(
    /// 一次等待用户回答的交互（权限、提问、计划审批）。
    InteractionId
);
string_id!(
    /// 一个 Artifact。
    ArtifactId
);
string_id!(
    /// 链路追踪 ID：一个顶层会话及其子会话、子代理、后台任务共用一个。
    TraceId
);
string_id!(
    /// shell 命令的任务 ID（`b` + 8 位 36 进制），也是输出文件名。
    ShellTaskId
);

/// 会话 owner 的租约版本。存储与执行环境拒绝旧版本的写入；本地只有一个 owner，恒为 1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnerEpoch(pub u64);

impl OwnerEpoch {
    pub const LOCAL: Self = Self(1);
}

/// 消息 ID 里创建毫秒的 36 进制位数上限（11 位够到公元 5 千年以后；更长的不是本格式）。
const MAX_CREATED_MS_DIGITS: usize = 11;
/// 消息 ID 里创建毫秒的进制。
const CREATED_MS_RADIX: u32 = 36;

/// Node 格式的消息 ID `msg_<36 进制毫秒>_<uuid>` 里的创建毫秒；格式不符时为 None。[`MessageId::created_ms`] 与
/// node-store 的 `message_time` 共用（工作线 F122：以前两边各写一份）。
pub fn message_created_ms(id: &str) -> Option<u64> {
    let digits = id.strip_prefix("msg_")?.split_once('_')?.0;
    if digits.is_empty() || digits.len() > MAX_CREATED_MS_DIGITS {
        return None;
    }
    u64::from_str_radix(digits, CREATED_MS_RADIX).ok()
}

impl MessageId {
    /// Node 格式 `msg_<36 进制毫秒>_<uuid>` 里的创建毫秒（node-store 写进 `time_created` 的同一个值）；
    /// 格式不符时为 None。resume 复原会话任务时取 CronCreate 所在 assistant 消息的时间（tools-more.md §18.2）。
    pub fn created_ms(&self) -> Option<u64> {
        message_created_ms(&self.0)
    }
}
