//! hook 的编排类型与 HTTP 出口端口（docs/specs/rust-cli/hooks.md §1、§5.2、§8）。
//!
//! 能力 crate（tools、query、session）经 [`HookRunner`] 触发事件；事件专属的处理（
//! 反馈文本、attachment 渲染）由实现方完成，结果放进 [`HookOutcome`]，调用方按字段提交消息、改变流程。
//! [`crate::orchestration`] 重新导出这里的类型，旧路径不变。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::env::{EnvPath, WorkspaceRef};
use crate::ids::{AgentId, RunId, SessionId};
use crate::message::Message;
use crate::permission::{PermissionDecision, PermissionMode};

macro_rules! hook_events {
    ($($name:ident),+ $(,)?) => {
        /// hook 事件名（33 个事件，顺序固定）。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum HookEventName {
            $($name),+
        }

        impl HookEventName {
            pub const ALL: &'static [HookEventName] = &[$(HookEventName::$name),+];

            /// 设置与输入 JSON 里的名字。
            pub fn as_str(self) -> &'static str {
                match self {
                    $(HookEventName::$name => stringify!($name)),+
                }
            }

            pub fn parse(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|event| event.as_str() == name)
            }
        }
    };
}

hook_events!(
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PostToolBatch,
    Notification,
    UserPromptSubmit,
    UserPromptExpansion,
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    PreModelSwitch,
    PostModelSwitch,
    PermissionRequest,
    PermissionDenied,
    Setup,
    TeammateIdle,
    TaskCreated,
    TaskCompleted,
    Elicitation,
    ElicitationResult,
    ConfigChange,
    WorktreeCreate,
    WorktreeRemove,
    InstructionsLoaded,
    CwdChanged,
    FileChanged,
    DirectoryAdded,
    MessageDisplay,
);

/// Node 的 7 个事件，按 Node `WORKSPACE_HOOK_EVENT_NAMES` 的顺序：项目 hook 的 digest 按这个顺序遍历，信任记录的
/// `eventAtGrant` 只允许它们（config.md §4）。F58：以前 hooks（枚举）与 platform（字符串）各写一份。
pub const NODE_EVENTS: [HookEventName; 7] = [
    HookEventName::SessionStart,
    HookEventName::UserPromptSubmit,
    HookEventName::PreToolUse,
    HookEventName::PermissionRequest,
    HookEventName::PostToolUse,
    HookEventName::PostToolUseFailure,
    HookEventName::Stop,
];

/// hook 输入的公共字段与命令 hook 的执行上下文。全部可缺省。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookContext {
    /// `permission_mode`：只有传模式的调用点（工具、Stop、UserPromptSubmit 等）才填。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    /// `agent_id`：在子代理里触发时有值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// `agent_type`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// `cwd`；没有时取会话的 shell cwd，再没有时取工作区根。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<EnvPath>,
    /// 触发 hook 的运行：命令 hook 的操作来源（`Origin::Agent`）；没有时来源是 `Origin::User`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    /// 触发时会话的当前主模型：prompt hook 的缺省模型经小模型槽位从它得出（docs/specs/rust-cli/
    /// model.md §6.1、hooks.md §5.3）。调用方没有填时 hook 引擎用配置的回退值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<crate::model::ModelSelection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub name: HookEventName,
    pub session: SessionId,
    pub workspace: WorkspaceRef,
    /// 事件专属字段（与 hook 输入 JSON 同形，键序即输入 JSON 的键序）。
    pub payload: Value,
    #[serde(default)]
    pub context: HookContext,
}

/// 所有匹配的 hook 运行后的合并结果。各字段的含义按事件见 docs/specs/rust-cli/hooks.md §6、§7。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HookOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PermissionDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_context: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    #[serde(default)]
    pub prevent_continuation: bool,
    /// `continue: false` 时 hook 给出的 `stopReason`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// 按提交顺序排好、要提交进会话、给模型看的消息（hook attachment 与 meta user 消息），
    /// 文本已渲染好。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// PreToolUse：排在工具结果之后的消息（`continue: false` 的停止说明）。其他事件为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after_result: Vec<Message>,
    /// 只给用户看的提示（`systemMessage`、UserPromptSubmit 的阻塞说明、PreCompact 的结果行等）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
    /// PostToolUse 的 `updatedToolOutput`：替换给模型的工具结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_tool_output: Option<Value>,
    /// SessionStart 的 `initialUserMessage`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_user_message: Option<String>,
    /// SessionStart / UserPromptSubmit 的 `sessionTitle`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// SessionStart 的 `watchPaths`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_paths: Vec<String>,
    /// PreCompact：成功的 hook 输出合成的压缩指令。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// PermissionDenied 的 `retry`。
    #[serde(default)]
    pub retry: bool,
}

#[async_trait]
pub trait HookRunner: Send + Sync {
    async fn run(&self, event: HookEvent, cancel: CancellationToken) -> HookOutcome;

    /// 当前配置里有没有这个事件的 hook（不看 matcher）。调用方用它决定要不要为 hook 进入等待状态
    /// （docs/specs/rust-cli/session-lifecycle.md §3.2）；默认按“有”处理，结果不变、只是多一次空跑。
    fn has_hooks(&self, _event: HookEventName) -> bool {
        true
    }

    /// 会话级的停止条件（`/goal`，docs/specs/rust-cli/goal.md §4、§5）：评估模型读对话记录判断条件。
    /// 缺省不支持，判定为出错（不阻塞）。
    async fn evaluate_stop_condition(
        &self,
        _request: StopCondition,
        _cancel: CancellationToken,
    ) -> StopVerdict {
        StopVerdict::Failed {
            message: "stop conditions are not supported".to_owned(),
        }
    }
}

/// 一次停止条件的评估（会话级的 Stop prompt hook）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopCondition {
    /// 这次 Stop 事件（输入 JSON 的来源）。
    pub event: HookEvent,
    pub condition: String,
    /// 活动历史按发给模型的规则 normalize 后的对话记录。
    pub transcript: Vec<crate::model::ApiMessage>,
}

/// 评估的判定（goal.md §5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum StopVerdict {
    Met {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// 未达成：`feedback` 是给模型的阻塞反馈（meta `stop_hook_feedback`）。
    NotMet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        feedback: Message,
    },
    Impossible {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// 评估出错（API 错误、输出不合 schema）：不阻塞。
    Failed {
        message: String,
    },
    TimedOut,
    Cancelled,
}

/// http hook 的一次 POST。header 已按 hook 规则插值与清洗；实现方只负责发送。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookHttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookHttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookHttpError {
    #[error("cancelled")]
    Cancelled,
    #[error("timed out")]
    TimedOut,
    #[error("{message}")]
    Failed { message: String },
}

/// http hook 的出口（执行 http hook 的网络部分）。实现方经 rust-cli-net 发出。
#[async_trait]
pub trait HookHttp: Send + Sync {
    async fn post(
        &self,
        request: HookHttpRequest,
        cancel: CancellationToken,
    ) -> Result<HookHttpResponse, HookHttpError>;
}

/// 一条并入的插话：文本与排在它的 `queued_command` 之后提交的 hook 消息。
#[derive(Debug, Clone, PartialEq)]
pub struct Steered {
    pub text: String,
    pub after: Vec<Message>,
}

impl Steered {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            after: Vec::new(),
        }
    }
}

/// 工具边界并入的一项（L18，docs/specs/rust-cli/query.md §5）：插话与通知按到达顺序交错（两者
/// 都是优先级 `next`，按队列顺序取）。
#[derive(Debug, Clone, PartialEq)]
pub enum Absorbed {
    /// 插话：`queued_command`（`commandMode: prompt`），之后提交它的 hook 消息。
    Prompt(Steered),
    /// 通知全文：`queued_command`（`commandMode: task-notification`）。
    Notice(String),
}

impl Absorbed {
    /// 没有到达顺序时的排列：先插话、后通知（`RunSink::take_absorbed` 的缺省）。
    pub fn ordered(steered: Vec<Steered>, notices: Vec<String>) -> Vec<Self> {
        let prompts = steered.into_iter().map(Self::Prompt);
        prompts
            .chain(notices.into_iter().map(Self::Notice))
            .collect()
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
