//! TUI 的会话视图模型（docs/specs/rust-cli/tui.md §4）：把 [`SessionView`] 与 [`KernelEvent`] 折叠成
//! 有序的行。视图是内核事实的投影，随时可以丢弃后从 `view()` 重建；这里不保存内核以外的会话事实。
//!
//! 行带视图内唯一的 `id` 与每次变化加一的 `version`，排版缓存按这两个值命中。
mod extras;
mod fire;
mod fold;
mod notice;

pub use extras::{Activity, AgentActivity, TurnChanges, task_counts};
pub use notice::Notice;

use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_api::{NoticeLevel, QueuedInput};
use serde_json::Value;

/// 回合的状态（回合行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Running,
    Success,
    Interrupted,
    Failed,
}

/// 工具调用的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// 模型已给出调用，还没开始执行。
    Pending,
    /// 等待用户审批。
    Approval,
    Running,
    Success,
    Error,
    /// 回合结束时还没有结果（中断）。
    Cancelled,
}

impl ToolStatus {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::Approval | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    pub id: ToolUseId,
    pub name: String,
    pub input: Value,
    pub status: ToolStatus,
    /// 结果的文字（tool_result 的 text 内容拼接）。
    pub output: String,
    /// 运行中的输出预览（Bash 的 `bash_progress`）。
    pub preview: String,
    /// 工具给 UI 的展示数据（`ToolOutput::display`，例如 Write 的 `type`）。
    pub display: Value,
    /// 前台子代理的活动（Agent 工具的进度，tui-display.md §2.1）；其他工具为 None。
    pub activity: Option<AgentActivity>,
}

/// 时间线标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Compacting,
    Compacted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// 回合头：不显示，用来分组与判断运行状态。
    Turn(TurnState),
    User {
        text: String,
    },
    Agent {
        text: String,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
        /// 推理用时；流式中为空，排版时按当前时间算。
        duration_ms: Option<u64>,
    },
    Tool(Box<ToolRow>),
    Marker(Marker),
    /// 回合失败的原因（红色）。
    Error(String),
    /// 本地命令的输出（`user-invocable: false` 的提示、展开失败的 stderr；dim 的 `⎿` 行，ext.md §2.7）。
    Output(String),
    /// 定时任务的触发行（`Running scheduled task`、`ZCode resuming /loop wakeup`，tools-more.md §18.3）。
    Fire(Box<Fire>),
    /// 只供显示的系统提示（`SystemNotice`，L18：插话被 UserPromptSubmit 阻塞）。
    Notice {
        level: NoticeLevel,
        text: String,
    },
    /// bash 模式的输入（`<bash-input>`，tui-input.md §2.1）。
    Shell {
        command: String,
    },
    /// bash 模式的输出（`<bash-stdout>` / `<bash-stderr>`，已转义的文字还原）。
    ShellOutput {
        stdout: String,
        stderr: String,
    },
    /// `/goal` 的评估结果（`goal_status`，不含设定 / 清除的标记，goal.md §6.1）。
    Goal(Box<rust_cli_kernel_api::goal::GoalStatus>),
    /// 跨会话：并入或开回合的同伴消息与发送方收到的回执（docs/specs/rust-cli/cross-session.md §2.6）。
    Peer(Box<PeerRow>),
    /// 回合结束那一行（`TurnDuration`，修复线 F118，tui-display.md §2.7）：只在实时事件里，冷加载没有（不落库）。
    Duration(Box<rust_cli_kernel_api::turn_duration::TurnDuration>),
}

/// 跨会话的一行（同伴消息与回执的警告行）。
#[derive(Debug, Clone, PartialEq)]
pub enum PeerRow {
    /// 同伴消息：发送方的名字（信封的 `from-name`，没有时是 socket 文件名或 `peer`）与正文。
    Message { name: String, body: String },
    /// 回执：`held` / `denied` / `expired` / `delivered` / `refused`。
    Receipt { status: String },
}

/// 一次触发（内核事件 `ScheduledFire`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub at: String,
    pub kind: rust_cli_kernel_api::cron::CronFireKind,
    pub fold: Option<rust_cli_kernel_api::cron::LoopFold>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: u64,
    pub version: u64,
    /// 行出现的时间（毫秒，调用方的时钟）。
    pub created_ms: u64,
    pub body: Body,
}

impl Row {
    pub fn tool(&self) -> Option<&ToolRow> {
        match &self.body {
            Body::Tool(tool) => Some(tool),
            _ => None,
        }
    }

    /// 还在变化的行（流式中的回复与推理、运行中的工具、运行中的回合）。
    pub fn live(&self) -> bool {
        match &self.body {
            Body::Turn(state) => *state == TurnState::Running,
            Body::Agent { streaming, .. } | Body::Reasoning { streaming, .. } => *streaming,
            Body::Tool(tool) => tool.status.is_live(),
            Body::User { .. }
            | Body::Marker(_)
            | Body::Error(_)
            | Body::Output(_)
            | Body::Fire(_)
            | Body::Notice { .. }
            | Body::Shell { .. }
            | Body::ShellOutput { .. }
            | Body::Goal(_)
            | Body::Peer(_)
            | Body::Duration(_) => false,
        }
    }
}

/// 响应段：一次模型响应的流式行从 `start` 开始；`blocks` 是内容块序号 → 行下标。
#[derive(Debug, Clone, Default)]
struct Segment {
    response: Option<u32>,
    start: usize,
    blocks: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    rows: Vec<Row>,
    next_id: u64,
    pending: Vec<(InteractionId, InteractionRequest)>,
    /// 扣住、等批准的同伴消息（快照的 `peer_held` 与 `PeerHeldChanged`，cross-session.md §2.6）。
    peer_held: Vec<rust_cli_kernel_api::peers::HeldPeerMessage>,
    queue: Vec<QueuedInput>,
    selection: ModelSelection,
    /// 执行状态：权限模式 × plan 开关（修复线 F20；`ModeChanged` / `PlanChanged` 各改一半）。
    execution: ExecutionState,
    /// 会话的工作区（快照的 `meta.workspace` 与 `WorkspaceChanged`）；没有会话时是新会话的草稿。前端的工作区只有
    /// 这一个来源（R1-3）。
    workspace: WorkspaceRef,
    title: String,
    /// 内核给出的“上下文快满”提示（`ContextNotice` 事件与快照，compact.md §2.1）。
    context: ContextNotice,
    /// 内核给出的上下文用量（`ContextUsage` 事件与快照的 `context_usage`）：footer 的 ctx（tui-display.md §2.6，F27）。
    occupancy: Option<ContextOccupancy>,
    /// 内核给出的最近一个完成的主回合请求的输出速度（`OutputSpeed` 事件与快照的 `output_speed`）：footer 的 TPS
    /// （tui-display.md §2.6，修复线 F105）。
    speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
    last_seq: u64,
    segment: Option<Segment>,
    /// 本回合流出的回复与推理字数（状态行的 `↓ N tokens`）。
    streamed_chars: usize,
    /// 任务清单、每回合改动、用量（L28，`extras.rs`）。
    extras: extras::Extras,
    /// 主线程还在生效的重试或停顿通知（L35，`notice.rs`）。
    notice: Option<Notice>,
}

impl Transcript {
    /// 还没有会话时的空视图（模型、执行状态与工作区是新会话的草稿）。单值的 `Plan` 是“plan 开、default”。
    pub fn new(
        selection: ModelSelection,
        execution: impl Into<ExecutionState>,
        workspace: WorkspaceRef,
    ) -> Self {
        Self {
            rows: Vec::new(),
            next_id: 1,
            pending: Vec::new(),
            peer_held: Vec::new(),
            queue: Vec::new(),
            selection,
            execution: execution.into(),
            workspace,
            title: String::new(),
            context: ContextNotice::Hidden,
            occupancy: None,
            speed: None,
            last_seq: 0,
            segment: None,
            streamed_chars: 0,
            extras: extras::Extras::default(),
            notice: None,
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// 最后收到的事件 seq（重新订阅的起点）。
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn selection(&self) -> &ModelSelection {
        &self.selection
    }

    /// 权限模式（不会是 plan；plan 看 [`Self::plan`]）。
    pub fn mode(&self) -> PermissionMode {
        self.execution.mode
    }

    /// plan 开关（修复线 F20）。
    pub fn plan(&self) -> bool {
        self.execution.plan
    }

    pub fn execution(&self) -> ExecutionState {
        self.execution
    }

    /// 没有会话时改新会话的草稿；有会话时以内核事件为准，调用方不用它。
    pub fn set_draft(&mut self, selection: ModelSelection, execution: ExecutionState) {
        self.selection = selection;
        self.execution = execution;
    }

    /// 会话的工作区；没有会话时是新会话的草稿。
    pub fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }

    /// 没有会话时 `/cd` 换新会话的工作区草稿；有会话时以 `WorkspaceChanged` 为准，调用方不用它。
    pub fn set_draft_workspace(&mut self, workspace: WorkspaceRef) {
        self.workspace = workspace;
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn queue(&self) -> &[QueuedInput] {
        &self.queue
    }

    /// 第一个待处理的交互（同时只显示一个）。
    /// 第一条扣住的同伴消息（批准框显示它，cross-session.md §2.6）。
    pub fn peer_held(&self) -> Option<&rust_cli_kernel_api::peers::HeldPeerMessage> {
        self.peer_held.first()
    }

    /// 扣住的条数。
    pub fn peer_held_count(&self) -> usize {
        self.peer_held.len()
    }

    pub fn pending(&self) -> Option<&(InteractionId, InteractionRequest)> {
        self.pending.first()
    }

    /// footer 的“上下文快满”提示（docs/specs/rust-cli/compact.md §2.1）：级别与数字由内核的压缩模块
    /// 算好，这里只取最新的一条。
    ///
    /// 缺陷修复（F14）：以前这里按 `(token, 有效窗口)` 自己算，常数写死（不看百分比覆盖），只有 `N% context used`
    /// 一种文字。
    pub fn context_notice(&self) -> ContextNotice {
        self.context
    }

    /// footer 的 ctx（tui-display.md §2.6，F27）：会话 actor 经压缩模块算的 `(token, 有效窗口)`，只取最新的一份；
    /// 没有会话或压缩模块不知道时为空。
    pub fn context_usage(&self) -> Option<ContextOccupancy> {
        self.occupancy
    }

    /// footer 的 TPS（tui-display.md §2.6，修复线 F105）：会话 actor 记的最近一个完成的主回合请求的速度；本进程还没有
    /// 这样的请求（冷打开、切换会话之后）时为空。
    pub fn output_speed(&self) -> Option<rust_cli_kernel_api::metering::OutputSpeed> {
        self.speed
    }

    /// 最后一个回合行：运行状态与开始时间。
    fn last_turn(&self) -> Option<(TurnState, u64)> {
        self.rows.iter().rev().find_map(|row| match row.body {
            Body::Turn(state) => Some((state, row.created_ms)),
            _ => None,
        })
    }

    /// 回合是否在运行（唯一依据：最后一个回合行，docs/specs/rust-cli/tui.md §4.3）。
    pub fn running(&self) -> bool {
        self.last_turn()
            .is_some_and(|(state, _)| state == TurnState::Running)
    }

    /// 运行中回合的开始时间。
    pub fn turn_started_ms(&self) -> Option<u64> {
        self.last_turn()
            .filter(|(state, _)| *state == TurnState::Running)
            .map(|(_, at)| at)
    }

    pub fn streamed_chars(&self) -> usize {
        self.streamed_chars
    }

    /// 是否有过会话内容（用户行）。
    pub fn has_messages(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.body, Body::User { .. } | Body::Shell { .. }))
    }

    /// 最后一条回复的正文（`/copy`）。
    pub fn last_reply(&self) -> Option<&str> {
        self.rows.iter().rev().find_map(|row| match &row.body {
            Body::Agent { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "duration_tests.rs"]
mod duration_tests;

/// 测试用的工作区（`/Users/dev/project`）。
#[cfg(test)]
pub(crate) fn test_workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
        root: rust_cli_kernel_api::env::EnvPath::new("/Users/dev/project"),
    }
}
